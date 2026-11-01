//! Live routing between the in-process GNU readline C ABI and the editor
//! (niubash#775).
//!
//! [`readline-abi`](readline) owns the exported `rl_*` state: its
//! `rl_line_buffer`/`rl_point`/`rl_end`/`rl_mark` globals, the readline
//! state bits, and the GNU-layout keymap arrays.  Before this module the
//! exported state was standalone: a loadable builtin could read and write a
//! private buffer, but the interactive Reedline editor never saw it.
//!
//! The directions wired here are:
//!
//! * **editor → ABI** at every host boundary (`read_line` start, a `bind -x`
//!   command, a widget, an accepted line).  The live buffer/point are copied
//!   into `rl_line_buffer`/`rl_point`/`rl_end`, and `rl_readline_state` and
//!   `rl_mark` are put in their boundary shape.
//! * **ABI → editor** after any host command that touched the ABI.  The
//!   exported buffer is read back as UTF-8 (invalid bytes are clamped to the
//!   valid prefix), the editor buffer/point are updated, and an active GNU
//!   mark becomes a selection.
//!
//! Plugin key registrations (`rl_add_defun`, `rl_bind_key*`,
//! `rl_unbind_*`) land in the ABI keymap arrays, not in a Rust callback the
//! editor could observe.  [`plugin_state`] snapshots those arrays right after
//! `rl_initialize` (the baseline) and diffs them against the live arrays.  A
//! changed slot becomes a host binding to a `__niu_readline_abi` sentinel;
//! the sentinel dispatches the plugin's C function and then applies the ABI
//! buffer back to the editor.  Because Reedline keymaps are baked at editor
//! build time, the REPL compares [`fingerprint`] between prompts and rebuilds
//! losslessly (the buffer is empty at that boundary) when a plugin registered
//! or removed a binding at runtime.
//!
//! The reverse, host → ABI direction is [`sync_host_keymap`]: the engine's
//! `bind` / `bind -x` / macro / inputrc entries are written into the exported
//! keymap arrays, so a loadable builtin's `rl_function_of_keyseq[_len]` sees
//! the same keymap the editor uses.  [`plugin_state`] treats host-owned slots
//! as part of its baseline, so a host bind never looks like a plugin bind and
//! a plugin rebind of the same key still does.
//!
//! The one-shot readline controls are consumed at a host-command resume:
//! [`take_pending_input`] injects the queued key, [`take_done`] submits the
//! line, [`begin_read`] runs the hooks in GNU order, and only a fresh
//! `readline()` resets `rl_mark`/`rl_done`/`rl_pending_input` (GNU
//! `rl_initialize`).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString};
use std::hash::{Hash, Hasher};
use std::os::raw::{c_char, c_int, c_ulong};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use reedline::{EditCommand, KeyCode, KeyModifiers, Reedline, ReedlineEvent};

use crate::keymap_registry::KeymapMode;
use crate::repl::NativeKeymapTarget;

/// Sentinel prefix carried by `ReedlineEvent::ExecuteHostCommand` for a key
/// a loadable builtin bound through the readline ABI.  The payload is
/// `"<root> <hex-path>"`.
pub const READLINE_ABI_HOST_COMMAND_PREFIX: &str = "__niu_readline_abi ";

const RL_STATE_READCMD: c_ulong = 0x0000_0008;
const RL_STATE_DISPATCHING: c_ulong = 0x0000_0020;
const RL_STATE_INPUTPENDING: c_ulong = 0x0002_0000;
const RL_STATE_DONE: c_ulong = 0x0200_0000;
const RL_STATE_EOF: c_ulong = 0x0800_0000;

const ROOT_EMACS: usize = 0;
const ROOT_VI_INSERT: usize = 1;
const ROOT_VI_MOVEMENT: usize = 2;

const MAX_KEYMAP_DEPTH: usize = 8;

// --- ABI state helpers ----------------------------------------------------

#[allow(dead_code)]
fn abi_line_buffer() -> *mut c_char {
    // SAFETY: reading the exported pointer is an atomic-sized read of a
    // process-wide global; callers use the result only while the ABI is not
    // being re-initialized.
    unsafe { ptr::addr_of!(readline::rl_line_buffer).read() }
}

fn abi_point() -> c_int {
    unsafe { ptr::addr_of!(readline::rl_point).read() }
}

fn abi_end() -> c_int {
    unsafe { ptr::addr_of!(readline::rl_end).read() }
}

fn abi_mark() -> c_int {
    unsafe { ptr::addr_of!(readline::rl_mark).read() }
}

fn set_abi_point(value: c_int) {
    unsafe { ptr::addr_of_mut!(readline::rl_point).write(value) };
}

fn set_abi_end(value: c_int) {
    unsafe { ptr::addr_of_mut!(readline::rl_end).write(value) };
}

fn set_abi_mark(value: c_int) {
    unsafe { ptr::addr_of_mut!(readline::rl_mark).write(value) };
}

fn set_abi_done(value: c_int) {
    unsafe { ptr::addr_of_mut!(readline::rl_done).write(value) };
}

fn set_abi_pending_input(value: c_int) {
    unsafe { ptr::addr_of_mut!(readline::rl_pending_input).write(value) };
}

fn deactivate_abi_mark() {
    // A fresh readline line starts with no active region, so an old plugin
    // selection must not leak into the new line.
    unsafe { readline::rl_deactivate_mark() };
}

fn readline_state() -> c_ulong {
    unsafe { ptr::addr_of!(readline::rl_readline_state).read() }
}

fn set_readline_state(value: c_ulong) {
    unsafe { ptr::addr_of_mut!(readline::rl_readline_state).write(value) };
}

fn add_state(bits: c_ulong) {
    set_readline_state(readline_state() | bits);
}

fn remove_state(bits: c_ulong) {
    set_readline_state(readline_state() & !bits);
}

/// Clamp a byte offset to `len` and then back to a UTF-8 character boundary.
fn clamp_boundary(text: &str, position: usize) -> usize {
    let mut position = position.min(text.len());
    while position > 0 && !text.is_char_boundary(position) {
        position -= 1;
    }
    position
}

/// Replace the exported line buffer with `text`, placing point/mark as
/// requested.  `mark == None` keeps the current exported mark (clamped).
fn write_abi_line(text: &str, point: usize, mark: Option<c_int>) {
    let bytes = text.as_bytes();
    // SAFETY: rl_extend_line_buffer grows the libc allocation used by the
    // exported pointer; all writes below stay inside the requested length.
    unsafe {
        readline::rl_extend_line_buffer(bytes.len() as c_int);
        let buffer = ptr::addr_of!(readline::rl_line_buffer).read();
        if buffer.is_null() {
            return;
        }
        // `rl_line_buffer` is `*mut c_char` and `c_char` is `u8` on Android
        // (making the cast look redundant) but `i8` on x86_64
        // Linux/macOS/Windows, where the `*mut u8` cast is required.
        #[allow(clippy::unnecessary_cast)]
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len());
        *buffer.add(bytes.len()) = 0;
    }
    let point = clamp_boundary(text, point);
    set_abi_end(bytes.len() as c_int);
    set_abi_point(point as c_int);
    let mark = match mark {
        Some(value) => value.clamp(-1, bytes.len() as c_int),
        None => abi_mark().clamp(-1, bytes.len() as c_int),
    };
    set_abi_mark(mark);
}

/// A copy of the exported line state (the unit plugin mutations are detected
/// against).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiLineSnapshot {
    pub text: String,
    pub point: usize,
    pub end: usize,
    pub mark: c_int,
}

/// Read the exported line as UTF-8.  A plugin is a C caller: a malformed
/// byte sequence is not allowed to poison the Rust editor, so only the valid
/// prefix is adopted (the same clamp the editor applies on write-back).
fn read_abi_line() -> AbiLineSnapshot {
    let end = abi_end().max(0) as usize;
    let buffer = abi_line_buffer();
    let mut text = String::new();
    if !buffer.is_null() && end > 0 {
        // SAFETY: rl_end is the number of valid bytes in rl_line_buffer and
        // the ABI always keeps a trailing NUL.
        let bytes = unsafe { std::slice::from_raw_parts(buffer as *const u8, end) };
        text = match std::str::from_utf8(bytes) {
            Ok(valid) => valid.to_string(),
            Err(error) => {
                let valid = &bytes[..error.valid_up_to()];
                String::from_utf8_lossy(valid).into_owned()
            }
        };
    }
    let point = clamp_boundary(&text, abi_point().max(0) as usize);
    let mark = abi_mark().clamp(-1, text.len() as c_int);
    let end = text.len();
    AbiLineSnapshot {
        text,
        point,
        end,
        mark,
    }
}

/// Copy the live editor state into the exported ABI globals.  This is the
/// editor → ABI half of the boundary bridge.
pub fn sync_editor_to_abi(buffer: &str, point: usize, mark: Option<c_int>) {
    write_abi_line(buffer, point, mark);
}

/// Apply the exported ABI line back to the live editor.  Returns whether the
/// editor buffer changed.  An active GNU mark is represented as a selection;
/// otherwise the point is restored without selecting.
pub fn sync_abi_to_editor(line_editor: &mut Reedline) -> bool {
    let snapshot = read_abi_line();
    let mut changed = false;

    if line_editor.current_buffer_contents() != snapshot.text {
        line_editor.run_edit_commands(&[
            EditCommand::Clear,
            EditCommand::InsertString(snapshot.text.clone()),
        ]);
        changed = true;
    }

    let mark_active = unsafe { readline::rl_mark_active_p() != 0 };
    let current_point = line_editor.current_insertion_point();
    if mark_active && snapshot.mark >= 0 && snapshot.mark as usize != snapshot.point {
        line_editor.run_edit_commands(&[
            EditCommand::MoveToPosition {
                position: snapshot.mark as usize,
                select: false,
            },
            EditCommand::MoveToPosition {
                position: snapshot.point,
                select: true,
            },
        ]);
        changed = true;
    } else if current_point != snapshot.point {
        line_editor.run_edit_commands(&[EditCommand::MoveToPosition {
            position: snapshot.point,
            select: false,
        }]);
        changed = true;
    }

    // Canonicalize the exported globals to what the editor actually adopted
    // (the plugin may have written a non-boundary point or an invalid tail).
    set_abi_end(snapshot.end as c_int);
    set_abi_point(snapshot.point as c_int);
    changed
}

/// Snapshot the exported line before a host command so the REPL can tell
/// whether the command (or a plugin it invoked) mutated it.
pub fn snapshot_abi_line() -> AbiLineSnapshot {
    read_abi_line()
}

pub fn abi_line_changed(before: &AbiLineSnapshot) -> bool {
    read_abi_line() != *before
}

/// A readline line is in progress (GNU: between `readline()`'s
/// `rl_initialize()` and its return).  A host-command keypress suspends
/// and resumes the editor *inside* one line, so the boundary code must
/// tell a resume apart from a fresh prompt: only a fresh `readline()`
/// resets `rl_done`/`rl_pending_input`/the mark, exactly like GNU's
/// `rl_initialize` (readline.c:1224); a resume consumes them instead.
static LINE_OPEN: AtomicBool = AtomicBool::new(false);
static STARTUP_HOOK_RUN: AtomicBool = AtomicBool::new(false);

/// Boundary entered just before the editor reads a line: mirror the live
/// buffer, set the prompt, run the readline hooks, and reset the one-shot
/// state only when this is a fresh `readline()` (not a host-command
/// resume).
pub fn begin_read(line_editor: &mut Reedline, prompt: &str) {
    if let Ok(prompt) = CString::new(prompt) {
        // SAFETY: rl_set_prompt copies the NUL-terminated string.  GNU
        // sets the prompt before `rl_initialize` (readline.c:377-379).
        unsafe { readline::rl_set_prompt(prompt.as_ptr()) };
    }
    let fresh_line = !LINE_OPEN.swap(true, Ordering::SeqCst);
    let buffer = line_editor.current_buffer_contents().to_string();
    let point = line_editor.current_insertion_point();
    if fresh_line {
        // GNU `_rl_init_line_state`: point = end = mark = 0, then
        // `rl_deactivate_mark` (readline.c:444, 781).
        sync_editor_to_abi(&buffer, point, Some(0));
        deactivate_abi_mark();
        set_abi_pending_input(0);
        set_abi_done(0);
        remove_state(RL_STATE_DONE | RL_STATE_EOF | RL_STATE_INPUTPENDING);
    } else {
        // Resume inside the same line: keep the plugin's mark alive and
        // carry a live editor selection back into `rl_mark`.
        let mark = line_editor
            .current_selection()
            .map(|(start, _)| start as c_int);
        sync_editor_to_abi(&buffer, point, mark);
    }
    add_state(RL_STATE_READCMD);

    // GNU ordering (readline.c:425-472): rl_initialize → startup hook →
    // deactivate mark → prompt display → pre-input hook → read the first
    // key.  A hook may edit `rl_line_buffer`, so write the result back to
    // the live editor before it starts reading.
    if fresh_line {
        // One `readline()` call per line, including PS2 continuation
        // prompts; a host-command resume stays inside the same call and
        // must not re-run the hooks.
        let before = read_abi_line();
        run_startup_hook_once();
        run_pre_input_hook();
        if read_abi_line() != before {
            sync_abi_to_editor(line_editor);
        }
    }
}

fn run_startup_hook_once() {
    if STARTUP_HOOK_RUN.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: reading a function-pointer global; the hook is a C callback
    // the host process installed and owns.
    let hook = unsafe { ptr::addr_of!(readline::rl_startup_hook).read() };
    if let Some(hook) = hook {
        unsafe { hook() };
    }
}

fn run_pre_input_hook() {
    // SAFETY: same contract as the startup hook; GNU runs it before every
    // line (readline.c:471).
    let hook = unsafe { ptr::addr_of!(readline::rl_pre_input_hook).read() };
    if let Some(hook) = hook {
        unsafe { hook() };
    }
}

/// Consume `rl_pending_input`: the plugin-queued key is returned once and
/// the global cleared, the GNU `rl_read_key` behaviour (input.c:786-790).
pub fn take_pending_input() -> Option<c_int> {
    let value = unsafe { ptr::addr_of!(readline::rl_pending_input).read() };
    if value == 0 {
        return None;
    }
    set_abi_pending_input(0);
    remove_state(RL_STATE_INPUTPENDING);
    Some(value)
}

/// Consume `rl_done`: a plugin that set it asked readline to return the
/// current line immediately.
pub fn take_done() -> bool {
    let value = unsafe { ptr::addr_of!(readline::rl_done).read() };
    if value == 0 {
        return false;
    }
    set_abi_done(0);
    true
}

/// Inject one plugin-queued key into the live editor.  Returns the line
/// when the key accepts it (`rl_pending_input = '\r'`), mirroring
/// `rl_read_key` → keymap dispatch for the common editing keys.  The
/// pinned Reedline exposes no public event-injection entry point, so the
/// keys readline dispatches through its default map are replayed as the
/// equivalent `EditCommand` here (see docs/audit/readline-live-abi.md).
pub fn apply_pending_key(line_editor: &mut Reedline, key: c_int) -> Option<String> {
    let byte = u8::try_from(key).ok()?;
    if byte == b'\r' || byte == b'\n' {
        return Some(line_editor.current_buffer_contents().to_string());
    }
    let command = match byte {
        0x00 => {
            // set-mark: GNU C-@ / C-SPC starts an active region at point.
            unsafe {
                readline::rl_set_mark(1, 0);
                readline::rl_activate_mark();
            }
            None
        }
        0x01 => Some(EditCommand::MoveToLineStart { select: false }),
        0x02 => Some(EditCommand::MoveLeft { select: false }),
        0x04 => Some(EditCommand::Delete),
        0x05 => Some(EditCommand::MoveToLineEnd { select: false }),
        0x06 => Some(EditCommand::MoveRight { select: false }),
        0x08 | 0x7f => Some(EditCommand::Backspace),
        0x09 => Some(EditCommand::Complete),
        0x0b => Some(EditCommand::CutToLineEnd),
        0x14 => Some(EditCommand::SwapGraphemes),
        0x15 => Some(EditCommand::CutFromLineStart),
        0x17 => Some(EditCommand::CutWordLeft),
        0x19 => Some(EditCommand::Yank),
        0x1f => Some(EditCommand::Undo),
        0x20..=0x7e => Some(EditCommand::InsertChar(byte as char)),
        _ => None,
    };
    if let Some(command) = command {
        line_editor.run_edit_commands(&[command]);
    }
    let buffer = line_editor.current_buffer_contents().to_string();
    let point = line_editor.current_insertion_point();
    sync_editor_to_abi(&buffer, point, None);
    None
}

/// Boundary after a line was accepted: the submitted text stays visible to a
/// command/plugin running after readline returns, exactly as GNU keeps the
/// accepted line in `rl_line_buffer` until the next `readline()` call.  A
/// live selection's anchor becomes `rl_mark` (GNU does not reset it on
/// accept), and the next fresh line resets it like `rl_initialize`.
pub fn finish_accepted(line: &str, mark: Option<c_int>) {
    sync_editor_to_abi(line, line.len(), Some(mark.unwrap_or(0)));
    LINE_OPEN.store(false, Ordering::SeqCst);
    set_abi_pending_input(0);
    remove_state(RL_STATE_READCMD | RL_STATE_DISPATCHING);
    add_state(RL_STATE_DONE);
    set_abi_done(1);
}

/// Boundary after Ctrl-C/Ctrl-D/eof: the line was abandoned.
pub fn finish_abandoned() {
    sync_editor_to_abi("", 0, Some(0));
    deactivate_abi_mark();
    LINE_OPEN.store(false, Ordering::SeqCst);
    set_abi_pending_input(0);
    remove_state(RL_STATE_READCMD | RL_STATE_DISPATCHING);
    add_state(RL_STATE_DONE);
    set_abi_done(1);
}

/// Bracket a plugin callback with GNU's `RL_STATE_DISPATCHING` bit.  A
/// plugin that inspects the state while running sees what readline would
/// show during command dispatch.
pub fn dispatch_state_begin() {
    add_state(RL_STATE_DISPATCHING);
}

pub fn dispatch_state_end() {
    remove_state(RL_STATE_DISPATCHING);
}

// --- Exported keymap diffs ------------------------------------------------

/// One leaf slot of an exported keymap trie.  Function slots compare by
/// pointer; macro slots compare by their byte text (`rl_macro_bind`
/// allocates a fresh C string on every bind, so pointer identity would
/// report a spurious plugin change after the host mirror rewrote the same
/// macro).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AbiSlot {
    Function(usize),
    Macro(Vec<u8>),
}

#[derive(Clone, Copy)]
struct RawEntry {
    kind: u8,
    function: usize,
}

impl RawEntry {
    fn read(map: *mut readline::KeymapEntry, key: usize) -> Self {
        // SAFETY: every map walked here is a KEYMAP_SIZE entry array owned
        // by the ABI (a built-in keymap or a libc allocation it returned).
        let entry = unsafe { ptr::read(map.add(key)) };
        Self {
            kind: entry.r#type,
            function: entry.function as usize,
        }
    }
}

fn root_map(root: usize) -> *mut readline::KeymapEntry {
    // The exported arrays live for the process lifetime.  Taking their
    // address (never a Rust reference) is the static_mut_refs-safe form.
    match root {
        ROOT_EMACS => ptr::addr_of_mut!(readline::emacs_standard_keymap).cast(),
        ROOT_VI_INSERT => ptr::addr_of_mut!(readline::vi_insertion_keymap).cast(),
        ROOT_VI_MOVEMENT => ptr::addr_of_mut!(readline::vi_movement_keymap).cast(),
        _ => ptr::null_mut(),
    }
}

fn root_mode(root: usize) -> KeymapMode {
    match root {
        ROOT_VI_INSERT => KeymapMode::ViInsert,
        ROOT_VI_MOVEMENT => KeymapMode::ViNormal,
        _ => KeymapMode::Emacs,
    }
}

fn root_keymap_name(root: usize) -> &'static str {
    match root {
        ROOT_VI_INSERT => "vi-insert",
        ROOT_VI_MOVEMENT => "vi-movement",
        _ => "emacs-standard",
    }
}

fn keymap_name_root(name: Option<&str>) -> usize {
    match name {
        Some("vi-insert") => ROOT_VI_INSERT,
        Some("vi") | Some("vi-move") | Some("vi-command") | Some("vi-movement") => ROOT_VI_MOVEMENT,
        // `emacs`, `emacs-standard`, `emacs-meta` and `emacs-ctlx`: the
        // host mirrors all four onto the single editor keymap whose keyseq
        // text carries the ESC / C-x prefix (`engine_keymap_applies`), so
        // the ABI mirror names the same one map.
        _ => ROOT_EMACS,
    }
}

fn macro_text(pointer: usize) -> Vec<u8> {
    if pointer == 0 {
        return Vec::new();
    }
    // SAFETY: an ISMACR slot holds a NUL-terminated libc allocation owned by
    // the ABI keymap; it stays valid until the slot is rebound.
    let bytes = unsafe { CStr::from_ptr(pointer as *const c_char) }.to_bytes();
    bytes.to_vec()
}

/// Walk one keymap trie and record every leaf slot by its raw key path.
/// Function and macro slots are both leaves; child keymaps are walked and
/// their slots recorded under the extended path.
fn collect_map(
    map: *mut readline::KeymapEntry,
    path: &mut Vec<u8>,
    out: &mut BTreeMap<(usize, Vec<u8>), AbiSlot>,
    root: usize,
    depth: usize,
) {
    if map.is_null() || depth > MAX_KEYMAP_DEPTH {
        return;
    }
    for key in 0..readline::KEYMAP_SIZE - 1 {
        let entry = RawEntry::read(map, key);
        match entry.kind {
            readline::ISFUNC if entry.function != 0 => {
                let mut full = path.clone();
                full.push(key as u8);
                out.insert((root, full), AbiSlot::Function(entry.function));
            }
            readline::ISMACR if entry.function != 0 => {
                let mut full = path.clone();
                full.push(key as u8);
                out.insert((root, full), AbiSlot::Macro(macro_text(entry.function)));
            }
            readline::ISKMAP if entry.function != 0 => {
                path.push(key as u8);
                collect_map(
                    entry.function as *mut readline::KeymapEntry,
                    path,
                    out,
                    root,
                    depth + 1,
                );
                path.pop();
            }
            _ => {}
        }
    }
    // A binding on a key that is itself a keymap prefix lands in that
    // child map's `ANYOTHERKEY` shadow slot (`_rl_dispatch_subseq` runs it
    // when the next key does not extend the sequence).  Report the shadow
    // under the prefix path so the host mirror and the plugin diff see the
    // same slot.
    if !path.is_empty() {
        let shadow = RawEntry::read(map, readline::ANYOTHERKEY);
        match shadow.kind {
            readline::ISFUNC if shadow.function != 0 => {
                out.insert((root, path.clone()), AbiSlot::Function(shadow.function));
            }
            readline::ISMACR if shadow.function != 0 => {
                out.insert(
                    (root, path.clone()),
                    AbiSlot::Macro(macro_text(shadow.function)),
                );
            }
            _ => {}
        }
    }
}

fn scan_maps() -> BTreeMap<(usize, Vec<u8>), AbiSlot> {
    let mut out = BTreeMap::new();
    for root in [ROOT_EMACS, ROOT_VI_INSERT, ROOT_VI_MOVEMENT] {
        let mut path = Vec::new();
        collect_map(root_map(root), &mut path, &mut out, root, 0);
    }
    out
}

/// Keymap snapshot keyed by `(root, key sequence)` -> ABI slot.
type KeymapSnapshot = BTreeMap<(usize, Vec<u8>), AbiSlot>;

static BASELINE: Mutex<Option<KeymapSnapshot>> = Mutex::new(None);
static INSTALLED_FINGERPRINT: Mutex<Option<u64>> = Mutex::new(None);
/// Host-owned keymap slots (engine `bind` / `bind -x` / macros / inputrc),
/// as last written into the ABI arrays.  `None` is an explicit host unbind
/// (`bind -r`), which is distinct from "the host has no opinion" (absent).
static HOST_MIRROR: Mutex<BTreeMap<(usize, Vec<u8>), Option<AbiSlot>>> =
    Mutex::new(BTreeMap::new());
/// Plugin macro binds already forwarded into the engine macro registry, so
/// the REPL does not re-register the same entry every prompt.
static FORWARDED_MACROS: Mutex<BTreeMap<(usize, Vec<u8>), Vec<u8>>> = Mutex::new(BTreeMap::new());

/// Capture the post-`rl_initialize` keymap state.  Must run before any rc
/// file, `enable -f`, or plugin can register a defun; the diff against this
/// snapshot is what distinguishes a plugin binding from a host default.
pub fn capture_baseline() {
    let baseline = scan_maps();
    if let Ok(mut slot) = BASELINE.lock() {
        if slot.is_none() {
            *slot = Some(baseline);
        }
    }
}

fn baseline() -> BTreeMap<(usize, Vec<u8>), AbiSlot> {
    if let Ok(slot) = BASELINE.lock() {
        if let Some(value) = slot.as_ref() {
            return value.clone();
        }
    }
    // Lazy fallback: only reached if the host forgot the explicit call.
    let captured = scan_maps();
    if let Ok(mut slot) = BASELINE.lock() {
        *slot = Some(captured.clone());
    }
    captured
}

fn host_mirror() -> BTreeMap<(usize, Vec<u8>), Option<AbiSlot>> {
    HOST_MIRROR
        .lock()
        .map(|mirror| mirror.clone())
        .unwrap_or_default()
}

/// The keymap the plugin diff compares against: the pristine ABI defaults
/// with the host mirror laid over them.  A host binding therefore never
/// looks like a plugin binding, while a plugin write over the same slot
/// still shows up as a change.
fn expected_maps() -> BTreeMap<(usize, Vec<u8>), AbiSlot> {
    let mut out = baseline();
    for (key, value) in host_mirror() {
        match value {
            Some(slot) => {
                out.insert(key, slot);
            }
            None => {
                out.remove(&key);
            }
        }
    }
    out
}

/// One plugin-owned key binding as the editor sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginKeyBinding {
    pub mode: KeymapMode,
    /// Reedline key units for the binding.  A Meta sequence is folded into
    /// one `ALT` combination; everything else keeps the GNU path length.
    pub keys: Vec<(KeyModifiers, KeyCode)>,
    /// `ReedlineEvent::ExecuteHostCommand` payload that dispatches it.
    pub payload: String,
}

/// One plugin-removed slot: the host mirror must drop the key the plugin
/// unbound rather than leave a stale editor binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginKeyUnbind {
    pub mode: KeymapMode,
    pub keys: Vec<(KeyModifiers, KeyCode)>,
}

/// One plugin macro (`rl_macro_bind`): the editor has no macro storage of
/// its own, so the host routes these into the engine's macro registry, the
/// same data plane a rc `bind '"\C-t": "text"'` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMacroBinding {
    pub mode: KeymapMode,
    pub root: usize,
    pub path: Vec<u8>,
    /// Macro payload as GNU stores it in the keymap (post-translation raw
    /// bytes; decoded lossily for the engine's UTF-8 macro table).
    pub text: String,
    /// GNU keyseq spelling the engine registry stores (`\C-t`, `\e[A`).
    pub keyseq: String,
}

#[derive(Debug, Clone, Default)]
pub struct PluginState {
    pub fingerprint: u64,
    pub bindings: Vec<PluginKeyBinding>,
    pub unbinds: Vec<PluginKeyUnbind>,
    pub macros: Vec<PluginMacroBinding>,
}

fn hex_path(path: &[u8]) -> String {
    let mut out = String::with_capacity(path.len() * 2);
    for byte in path {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn parse_path(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut path = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    for pair in bytes.as_chunks::<2>().0 {
        let text = std::str::from_utf8(pair).ok()?;
        path.push(u8::from_str_radix(text, 16).ok()?);
    }
    Some(path)
}

fn payload_for(root: usize, path: &[u8]) -> String {
    format!(
        "{READLINE_ABI_HOST_COMMAND_PREFIX}{root} {}",
        hex_path(path)
    )
}

/// Parse a sentinel payload back into `(root, path)`.
pub fn parse_payload(payload: &str) -> Option<(usize, Vec<u8>)> {
    let rest = payload.strip_prefix(READLINE_ABI_HOST_COMMAND_PREFIX)?;
    let mut fields = rest.split_whitespace();
    let root = fields.next()?.parse().ok()?;
    let path = parse_path(fields.next()?)?;
    if fields.next().is_some() || path.is_empty() {
        return None;
    }
    Some((root, path))
}

/// Map a GNU raw key byte to the Reedline key the terminal delivers for it.
fn key_from_byte(byte: u8) -> (KeyModifiers, KeyCode) {
    use KeyModifiers as M;
    match byte {
        0x1b => (M::NONE, KeyCode::Esc),
        0x09 => (M::NONE, KeyCode::Tab),
        0x0d => (M::NONE, KeyCode::Enter),
        0x7f => (M::NONE, KeyCode::Backspace),
        0x00 => (M::CONTROL, KeyCode::Char('@')),
        0x01..=0x1a => (M::CONTROL, KeyCode::Char((b'a' + byte - 1) as char)),
        0x1c => (M::CONTROL, KeyCode::Char('\\')),
        0x1d => (M::CONTROL, KeyCode::Char(']')),
        0x1e => (M::CONTROL, KeyCode::Char('^')),
        0x1f => (M::CONTROL, KeyCode::Char('_')),
        0x20..=0x7e => (M::NONE, KeyCode::Char(byte as char)),
        other => (
            M::NONE,
            KeyCode::Char(char::from_u32(other as u32).unwrap_or('\u{fffd}')),
        ),
    }
}

/// Convert a raw GNU key path to Reedline key units.  A two-byte ESC path is
/// one Meta combination — that is how reedline's input layer delivers it.
fn keys_from_path(path: &[u8]) -> Option<Vec<(KeyModifiers, KeyCode)>> {
    if path.is_empty() {
        return None;
    }
    if path.len() == 2 && path[0] == 0x1b {
        let (modifiers, code) = key_from_byte(path[1]);
        return Some(vec![(modifiers | KeyModifiers::ALT, code)]);
    }
    Some(path.iter().map(|byte| key_from_byte(*byte)).collect())
}

/// Render a raw GNU key path back into the inputrc/bind spelling the engine
/// registry speaks.  `rl_untranslate_keyseq` owns the per-byte encoding
/// (`\C-x`, `\e`, `\M-~`), so the spelling round-trips through the ABI's own
/// `rl_translate_keyseq`.
fn gnu_spelling(path: &[u8]) -> String {
    let mut out = String::with_capacity(path.len() * 4);
    for byte in path {
        // SAFETY: rl_untranslate_keyseq returns a static C buffer valid
        // until the next call; it is copied out immediately.
        let pointer = unsafe { readline::rl_untranslate_keyseq(*byte as c_int) };
        if pointer.is_null() {
            continue;
        }
        let text = unsafe { CStr::from_ptr(pointer) };
        out.push_str(&text.to_string_lossy());
    }
    out
}

/// Diff the live keymaps against the pristine-defaults-plus-host-mirror
/// expectation and return the plugin-owned bindings, removed slots, macros,
/// and a fingerprint.  The fingerprint is stable across calls with no
/// keymap movement, so the REPL can rebuild only when a plugin actually
/// registered or removed something.
pub fn plugin_state() -> PluginState {
    let expected = expected_maps();
    let current = scan_maps();
    let mut keys: BTreeSet<(usize, Vec<u8>)> = BTreeSet::new();
    keys.extend(expected.keys().cloned());
    keys.extend(current.keys().cloned());

    let mut state = PluginState::default();
    let mut hash = std::collections::hash_map::DefaultHasher::new();

    for (root, path) in keys {
        let before = expected.get(&(root, path.clone()));
        let after = current.get(&(root, path.clone()));
        if before == after {
            continue;
        }
        (root, path.clone()).hash(&mut hash);
        after.hash(&mut hash);

        let Some(keys) = keys_from_path(&path) else {
            continue;
        };
        match after {
            Some(AbiSlot::Macro(text)) => state.macros.push(PluginMacroBinding {
                mode: root_mode(root),
                root,
                keyseq: gnu_spelling(&path),
                text: String::from_utf8_lossy(text).into_owned(),
                path,
            }),
            Some(AbiSlot::Function(_)) => state.bindings.push(PluginKeyBinding {
                mode: root_mode(root),
                keys,
                payload: payload_for(root, &path),
            }),
            None => state.unbinds.push(PluginKeyUnbind {
                mode: root_mode(root),
                keys,
            }),
        }
    }

    state.fingerprint = hash.finish();
    state
}

/// Plugin macros that still need to join the engine macro registry (a
/// plugin bound them with `rl_macro_bind` and the host has not forwarded
/// that exact text yet).  The REPL registers each one as an ordinary
/// engine `bind` macro, which both installs it in the editor keymap on the
/// next build and makes it visible to `bind -s` / `bind -p`.
pub fn plugin_macros_pending() -> Vec<PluginMacroBinding> {
    let forwarded = FORWARDED_MACROS
        .lock()
        .map(|map| map.clone())
        .unwrap_or_default();
    plugin_state()
        .macros
        .into_iter()
        .filter(|binding| {
            let text = binding.text.clone().into_bytes();
            forwarded
                .get(&(binding.root, binding.path.clone()))
                .map(Vec::as_slice)
                != Some(text.as_slice())
        })
        .collect()
}

/// Record that `plugin_macros_pending`'s entries were handed to the engine,
/// so a rejected/ignored spelling cannot cause a rebuild loop.
pub fn mark_plugin_macros_forwarded(macros: &[PluginMacroBinding]) {
    if let Ok(mut forwarded) = FORWARDED_MACROS.lock() {
        for binding in macros {
            forwarded.insert(
                (binding.root, binding.path.clone()),
                binding.text.clone().into_bytes(),
            );
        }
    }
}

/// Current plugin-state fingerprint (see [`plugin_state`]).
pub fn fingerprint() -> u64 {
    plugin_state().fingerprint
}

/// Record the keymap generation the editor was just built from.
pub fn mark_installed() {
    if let Ok(mut slot) = INSTALLED_FINGERPRINT.lock() {
        *slot = Some(fingerprint());
    }
}

/// Whether a plugin changed the keymaps since the last editor build.
pub fn is_stale() -> bool {
    let current = fingerprint();
    match INSTALLED_FINGERPRINT.lock() {
        Ok(slot) => slot.is_none_or(|installed| installed != current),
        Err(_) => true,
    }
}

// --- Host keymap -> ABI mirror --------------------------------------------

/// A C-callable body for the host commands the ABI funmap does not name.
/// `rl_function_of_keyseq` consumers see a non-null command with
/// `kind == ISFUNC`; the pointer is inert on purpose (the live editor, not
/// the ABI dispatcher, owns execution).
unsafe extern "C" fn host_command_sentinel(_count: c_int, _key: c_int) -> c_int {
    0
}

static HOST_FUNMAP_REGISTERED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn host_command_pointer() -> usize {
    host_command_sentinel as *const () as usize
}

fn host_function_pointer(name: &str) -> usize {
    if let Ok(cname) = CString::new(name) {
        // SAFETY: rl_named_function only reads the NUL-terminated name.
        let function = unsafe { readline::rl_named_function(cname.as_ptr()) };
        if let Some(function) = function {
            return function as usize;
        }
        // bash adds its own commands (`shell-expand-line`,
        // `glob-complete-word`, …) with rl_add_defun; the ABI funmap does
        // not know them.  Materialize the name once so a plugin that looks
        // up `rl_named_function("shell-expand-line")` gets the same slot as
        // `rl_function_of_keyseq`.
        let register = HOST_FUNMAP_REGISTERED
            .lock()
            .map(|mut names| names.insert(name.to_string()))
            .unwrap_or(false);
        if register {
            unsafe {
                readline::rl_add_defun(cname.as_ptr(), Some(host_command_sentinel), -1);
            }
        }
    }
    host_command_pointer()
}

/// Translate a GNU keyseq spelling into the raw bytes `rl_function_of_keyseq`
/// consumes, through the ABI's own inputrc translator.
fn translate_keyseq(keyseq: &str) -> Option<Vec<u8>> {
    let cseq = CString::new(keyseq).ok()?;
    let mut buffer = [0 as c_char; 256];
    let mut length: c_int = 0;
    // SAFETY: the buffer is 256 bytes (GNU MAX_KEYSEQ), and rl_translate_keyseq
    // writes at most that many plus the NUL.
    let rc =
        unsafe { readline::rl_translate_keyseq(cseq.as_ptr(), buffer.as_mut_ptr(), &mut length) };
    if rc != 0 || length <= 0 || length as usize > buffer.len() {
        return None;
    }
    Some(
        buffer[..length as usize]
            .iter()
            .map(|byte| *byte as u8)
            .collect(),
    )
}

/// Spell raw key bytes with octal escapes: every byte round-trips through
/// `rl_translate_keyseq` unchanged, including 0x80..=0xff (which the
/// `\M-` form would fold through the ESC meta prefix) and backslash.
fn octal_spelling(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 5);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "\\{byte:03o}");
    }
    out
}

fn bind_raw_function(root: usize, path: &[u8], function: Option<usize>) -> bool {
    let spelling = octal_spelling(path);
    let Ok(cseq) = CString::new(spelling) else {
        return false;
    };
    let function = function.map(|pointer| {
        // SAFETY: a keymap ISFUNC slot holds an `int (*)(int, int)`; the
        // pointer came from rl_named_function, rl_add_defun or the ABI's
        // own default table.
        unsafe {
            std::mem::transmute::<usize, unsafe extern "C" fn(c_int, c_int) -> c_int>(pointer)
        }
    });
    // SAFETY: cseq is NUL-terminated; the map is one of the exported roots.
    unsafe { readline::rl_bind_keyseq_in_map(cseq.as_ptr(), function, root_map(root)) == 0 }
}

fn bind_raw_macro(root: usize, path: &[u8], text: &[u8]) -> bool {
    let spelling = octal_spelling(path);
    let payload = octal_spelling(text);
    let (Ok(cseq), Ok(cbody)) = (CString::new(spelling), CString::new(payload)) else {
        return false;
    };
    // SAFETY: both strings are NUL-terminated; rl_macro_bind copies them.
    unsafe { readline::rl_macro_bind(cseq.as_ptr(), cbody.as_ptr(), root_map(root)) == 0 }
}

/// Mirror the engine's `bind` / `bind -x` / macro / inputrc entries into the
/// exported ABI keymaps, and undo entries the host has since rebound or
/// removed.  Only *changed* host slots are written, so a plugin binding on
/// an untouched host key survives; a later host rebind of that key wins,
/// like GNU's last-write rule.
pub fn sync_host_keymap(
    entries: &[crate::bind_surface::EngineBindEntry],
    removals: &[(String, Option<String>)],
) -> bool {
    let mut desired: BTreeMap<(usize, Vec<u8>), Option<AbiSlot>> = BTreeMap::new();

    for (keyseq, keymap) in removals {
        let root = keymap_name_root(keymap.as_deref());
        if let Some(path) = translate_keyseq(keyseq) {
            if !path.is_empty() {
                desired.insert((root, path), None);
            }
        }
    }
    for entry in entries {
        let root = keymap_name_root(entry.keymap.as_deref());
        let Some(path) = translate_keyseq(&entry.keyseq) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let value = match &entry.kind {
            crate::bind_surface::EngineBindKind::Function { name } => {
                AbiSlot::Function(host_function_pointer(name))
            }
            crate::bind_surface::EngineBindKind::Execute { .. } => {
                AbiSlot::Function(host_command_pointer())
            }
            crate::bind_surface::EngineBindKind::Macro { text } => {
                AbiSlot::Macro(text.as_bytes().to_vec())
            }
        };
        desired.insert((root, path), Some(value));
    }

    let Ok(mut mirror) = HOST_MIRROR.lock() else {
        return false;
    };
    let mut changed = false;

    let stale: Vec<(usize, Vec<u8>)> = mirror
        .keys()
        .filter(|key| !desired.contains_key(*key))
        .cloned()
        .collect();
    let pristine = baseline();
    for (root, path) in stale {
        // The host dropped its opinion about this key (a cleared `bind -r`
        // tombstone, or an entry removed from the registry): put the
        // pristine ABI default back, or unbind a slot the defaults never
        // filled.
        let applied = match pristine.get(&(root, path.clone())) {
            Some(AbiSlot::Function(function)) => bind_raw_function(root, &path, Some(*function)),
            Some(AbiSlot::Macro(text)) => bind_raw_macro(root, &path, text),
            None => bind_raw_function(root, &path, None),
        };
        if applied {
            changed = true;
        }
        mirror.remove(&(root, path));
    }

    for (key, value) in desired {
        if mirror.get(&key) == Some(&value) {
            continue;
        }
        let applied = match &value {
            Some(AbiSlot::Function(function)) => bind_raw_function(key.0, &key.1, Some(*function)),
            Some(AbiSlot::Macro(text)) => bind_raw_macro(key.0, &key.1, text),
            None => bind_raw_function(key.0, &key.1, None),
        };
        if applied {
            changed = true;
            mirror.insert(key, value);
        }
    }

    changed
}

/// The keymap name a plugin macro at `root` should be registered under.
pub fn root_keymap_for_registry(root: usize) -> &'static str {
    root_keymap_name(root)
}

fn look_up(root: usize, path: &[u8]) -> Option<usize> {
    let mut map = root_map(root);
    if map.is_null() || path.is_empty() {
        return None;
    }
    for (index, byte) in path.iter().enumerate() {
        let entry = RawEntry::read(map, *byte as usize);
        if index + 1 == path.len() {
            return (entry.kind == readline::ISFUNC && entry.function != 0)
                .then_some(entry.function);
        }
        if entry.kind != readline::ISKMAP || entry.function == 0 {
            return None;
        }
        map = entry.function as *mut readline::KeymapEntry;
    }
    None
}

/// Dispatch the plugin function for `(root, path)`.  Returns false when the
/// slot has since been unbound or changed to a macro, so the caller can
/// ignore the stale sentinel instead of firing the wrong command.
pub fn dispatch(root: usize, path: &[u8]) -> bool {
    let Some(function) = look_up(root, path) else {
        return false;
    };
    let key = path.last().copied().unwrap_or(0) as c_int;
    // SAFETY: rl_add_defun/rl_bind_key only accept a `rl_command_func_t`
    // (int (*)(int, int)), and the ABI stored exactly that pointer.
    let callback: unsafe extern "C" fn(c_int, c_int) -> c_int = unsafe {
        std::mem::transmute::<usize, unsafe extern "C" fn(c_int, c_int) -> c_int>(function)
    };
    unsafe { callback(1, key) };
    true
}

/// Flat (single-key) plugin bindings for one editor keymap.
pub(crate) fn plugin_flat_bindings(target: NativeKeymapTarget) -> Vec<PluginKeyBinding> {
    let mode = mode_for_target(target);
    plugin_state()
        .bindings
        .into_iter()
        .filter(|binding| binding.mode == mode && binding.keys.len() == 1)
        .collect()
}

/// Single-key plugin unbinds for one editor keymap.
pub(crate) fn plugin_flat_unbinds(target: NativeKeymapTarget) -> Vec<PluginKeyUnbind> {
    let mode = mode_for_target(target);
    plugin_state()
        .unbinds
        .into_iter()
        .filter(|unbind| unbind.mode == mode && unbind.keys.len() == 1)
        .collect()
}

/// Multi-key plugin unbinds for one editor keymap.  The chord layer filters
/// these out after its default/host entries are assembled.
pub(crate) fn plugin_chord_unbinds(
    target: NativeKeymapTarget,
) -> Vec<Vec<(KeyModifiers, KeyCode)>> {
    let mode = mode_for_target(target);
    plugin_state()
        .unbinds
        .into_iter()
        .filter(|unbind| unbind.mode == mode && unbind.keys.len() >= 2)
        .map(|unbind| unbind.keys)
        .collect()
}

/// Multi-key plugin bindings for one editor keymap, as the chord layer's
/// `(keys, event)` pairs.
pub(crate) fn plugin_chords(
    target: NativeKeymapTarget,
) -> Vec<(Vec<(KeyModifiers, KeyCode)>, ReedlineEvent)> {
    let mode = mode_for_target(target);
    plugin_state()
        .bindings
        .into_iter()
        .filter(|binding| binding.mode == mode && binding.keys.len() >= 2)
        .map(|binding| {
            (
                binding.keys,
                ReedlineEvent::ExecuteHostCommand(binding.payload),
            )
        })
        .collect()
}

fn mode_for_target(target: NativeKeymapTarget) -> KeymapMode {
    match target {
        NativeKeymapTarget::Emacs => KeymapMode::Emacs,
        NativeKeymapTarget::ViInsert => KeymapMode::ViInsert,
        NativeKeymapTarget::ViNormal => KeymapMode::ViNormal,
    }
}

/// Apply the flat plugin layer to a host keymap.  Called from the editor
/// build's script layer, before the user's `NIU_BINDKEYS` layer, so a user
/// binding still wins like GNU's last-write rule.
pub(crate) fn add_plugin_keybindings(
    keybindings: &mut reedline::Keybindings,
    target: NativeKeymapTarget,
) {
    for unbind in plugin_flat_unbinds(target) {
        let (modifiers, code) = unbind.keys[0];
        keybindings.remove_binding(modifiers, code);
    }
    for binding in plugin_flat_bindings(target) {
        let (modifiers, code) = binding.keys[0];
        keybindings.add_binding(
            modifiers,
            code,
            ReedlineEvent::ExecuteHostCommand(binding.payload),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_bytes_map_to_the_terminal_chord() {
        assert_eq!(
            key_from_byte(0x07),
            (KeyModifiers::CONTROL, KeyCode::Char('g'))
        );
        assert_eq!(key_from_byte(0x09), (KeyModifiers::NONE, KeyCode::Tab));
        assert_eq!(
            key_from_byte(0x7f),
            (KeyModifiers::NONE, KeyCode::Backspace)
        );
        assert_eq!(
            key_from_byte(0xfe),
            (KeyModifiers::NONE, KeyCode::Char('\u{fe}'))
        );
    }

    #[test]
    fn meta_path_folds_to_one_alt_binding() {
        assert_eq!(
            keys_from_path(&[0x1b, b'f']),
            Some(vec![(KeyModifiers::ALT, KeyCode::Char('f'))])
        );
        assert_eq!(
            keys_from_path(&[0x18, b'r']),
            Some(vec![
                (KeyModifiers::CONTROL, KeyCode::Char('x')),
                (KeyModifiers::NONE, KeyCode::Char('r')),
            ])
        );
    }

    #[test]
    fn payload_round_trips() {
        let payload = payload_for(ROOT_EMACS, &[0xfe]);
        assert_eq!(parse_payload(&payload), Some((ROOT_EMACS, vec![0xfe])));
        assert_eq!(parse_payload("__niu_readline_abi zz 00"), None);
        assert_eq!(parse_payload("__niu_readline_abi 0 0"), None);
    }

    // The ABI keymaps and line globals are process-wide; serialize the
    // tests that mutate them and initialize/capture the pristine baseline
    // exactly once, mirroring `main()`'s startup order.
    static ABI_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_abi() -> std::sync::MutexGuard<'static, ()> {
        let guard = ABI_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _ = unsafe { readline::rl_initialize() };
        capture_baseline();
        guard
    }

    #[test]
    fn octal_spelling_round_trips_every_byte() {
        let _guard = lock_abi();
        for byte in 0u8..=255 {
            let spelling = octal_spelling(&[byte]);
            let translated = translate_keyseq(&spelling).expect("translated byte");
            assert_eq!(translated, vec![byte], "byte {byte:#04x} -> {spelling}");
        }
    }

    #[test]
    fn host_bind_is_visible_to_function_of_keyseq_and_reverted() {
        let _guard = lock_abi();
        let keyseq = "\\C-o";
        let path = translate_keyseq(keyseq).unwrap();
        let named = unsafe {
            let name = CString::new("dump-variables").unwrap();
            readline::rl_named_function(name.as_ptr()).expect("dump-variables")
        };
        let entries = vec![crate::bind_surface::EngineBindEntry {
            keyseq: keyseq.to_string(),
            kind: crate::bind_surface::EngineBindKind::Function {
                name: "dump-variables".to_string(),
            },
            keymap: Some("emacs-standard".to_string()),
        }];
        assert!(sync_host_keymap(&entries, &[]));
        let seen = unsafe {
            // `rl_function_of_keyseq` takes RAW key bytes (GNU does not
            // translate a bind spelling there).
            let raw = CString::new(path.clone()).unwrap();
            let mut kind: c_int = -1;
            readline::rl_function_of_keyseq(raw.as_ptr(), ptr::null_mut(), &mut kind)
        };
        assert_eq!(seen.map(|function| function as usize), Some(named as usize));
        assert_eq!(
            plugin_state().bindings,
            Vec::new(),
            "the host mirror must not look like a plugin bind"
        );

        // `bind -r`: the tombstone clears the slot for ABI consumers.
        assert!(sync_host_keymap(
            &[],
            &[(keyseq.to_string(), Some("emacs-standard".to_string()))]
        ));
        let cleared = unsafe {
            let raw = CString::new(path.clone()).unwrap();
            readline::rl_function_of_keyseq(raw.as_ptr(), ptr::null_mut(), ptr::null_mut())
        };
        assert!(cleared.is_none(), "the tombstone clears the slot");
        assert!(
            plugin_state().unbinds.is_empty(),
            "host tombstone is not a plugin unbind"
        );

        // Removing the tombstone restores the pristine default.
        assert!(sync_host_keymap(&[], &[]));
        let restored = unsafe {
            let raw = CString::new(path.clone()).unwrap();
            readline::rl_function_of_keyseq(raw.as_ptr(), ptr::null_mut(), ptr::null_mut())
        };
        assert!(restored.is_some(), "default C-o binding restored");
        assert_ne!(
            restored.map(|function| function as usize),
            Some(named as usize)
        );
    }

    #[test]
    fn plugin_macro_is_reported_then_forwarded_once() {
        let _guard = lock_abi();
        let keyseq = "\\C-t";
        let path = translate_keyseq(keyseq).unwrap();
        let pristine = baseline().get(&(ROOT_EMACS, path.clone())).cloned();
        unsafe {
            let sequence = CString::new(octal_spelling(&path)).unwrap();
            let text = CString::new("MAGIC").unwrap();
            assert_eq!(
                readline::rl_macro_bind(
                    sequence.as_ptr(),
                    text.as_ptr(),
                    ptr::addr_of_mut!(readline::emacs_standard_keymap).cast(),
                ),
                0
            );
        }
        let macros = plugin_state().macros;
        let found = macros
            .iter()
            .find(|binding| binding.path == path)
            .expect("plugin macro reported");
        assert_eq!(found.text, "MAGIC");
        assert_eq!(found.keyseq, "\\C-t");

        let pending = plugin_macros_pending();
        assert!(pending.iter().any(|binding| binding.path == path));
        mark_plugin_macros_forwarded(&pending);
        assert!(
            !plugin_macros_pending()
                .iter()
                .any(|binding| binding.path == path),
            "forwarded macro is not offered again"
        );

        // Restore the pristine C-t binding so other tests (and the host
        // keymap build) see the default again.
        unsafe {
            let sequence = CString::new(octal_spelling(&path)).unwrap();
            let function = match pristine {
                Some(AbiSlot::Function(function)) => Some(std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(c_int, c_int) -> c_int,
                >(function)),
                _ => None,
            };
            readline::rl_bind_keyseq_in_map(
                sequence.as_ptr(),
                function,
                ptr::addr_of_mut!(readline::emacs_standard_keymap).cast(),
            );
        }
        if let Ok(mut forwarded) = FORWARDED_MACROS.lock() {
            forwarded.remove(&(ROOT_EMACS, path));
        }
    }

    #[test]
    fn plugin_unbind_is_reported() {
        let _guard = lock_abi();
        let path = translate_keyseq("\\C-b").unwrap();
        unsafe {
            let sequence = CString::new(octal_spelling(&path)).unwrap();
            assert_eq!(
                readline::rl_bind_keyseq_in_map(
                    sequence.as_ptr(),
                    None,
                    ptr::addr_of_mut!(readline::emacs_standard_keymap).cast(),
                ),
                0
            );
        }
        let unbinds = plugin_state().unbinds;
        assert!(unbinds.iter().any(|unbind| unbind.mode == KeymapMode::Emacs
            && unbind.keys == vec![(KeyModifiers::CONTROL, KeyCode::Char('b'))]));

        // Restore backward-char for the other tests.
        let function = unsafe {
            let name = CString::new("backward-char").unwrap();
            readline::rl_named_function(name.as_ptr()).expect("backward-char")
        };
        unsafe {
            let sequence = CString::new(octal_spelling(&path)).unwrap();
            readline::rl_bind_keyseq_in_map(
                sequence.as_ptr(),
                Some(function),
                ptr::addr_of_mut!(readline::emacs_standard_keymap).cast(),
            );
        }
    }

    #[test]
    fn pending_input_and_done_are_consumed_once() {
        let _guard = lock_abi();
        set_abi_pending_input('Z' as c_int);
        assert_eq!(take_pending_input(), Some('Z' as c_int));
        assert_eq!(take_pending_input(), None);

        set_abi_done(1);
        assert!(take_done());
        assert!(!take_done());
    }

    #[test]
    fn pending_key_injects_printable_and_control_keys() {
        let _guard = lock_abi();
        let mut editor = Reedline::create();
        editor.run_edit_commands(&[EditCommand::InsertString("ab".to_string())]);
        assert_eq!(apply_pending_key(&mut editor, 'X' as c_int), None);
        assert_eq!(editor.current_buffer_contents(), "abX");
        assert_eq!(apply_pending_key(&mut editor, 0x01), None); // C-a
        assert_eq!(editor.current_insertion_point(), 0);
        let accepted = apply_pending_key(&mut editor, '\r' as c_int);
        assert_eq!(accepted.as_deref(), Some("abX"));
    }

    #[test]
    fn mark_survives_a_resume_but_not_a_fresh_line() {
        let _guard = lock_abi();
        let mut editor = Reedline::create();
        editor.run_edit_commands(&[EditCommand::InsertString("abc".to_string())]);
        begin_read(&mut editor, "P> ");
        set_abi_mark(2);
        // The same line resumed by a host-command keypress keeps the mark.
        begin_read(&mut editor, "P> ");
        assert_eq!(abi_mark(), 2);
        // A fresh readline() resets it like rl_initialize.
        finish_accepted("abc", Some(2));
        begin_read(&mut editor, "P> ");
        assert_eq!(abi_mark(), 0);
        finish_abandoned();
    }
}
