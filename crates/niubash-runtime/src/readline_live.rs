//! Live routing between the in-process GNU readline C ABI and the editor
//! (niubash#775).
//!
//! [`readline-abi`](readline) owns the exported `rl_*` state: its
//! `rl_line_buffer`/`rl_point`/`rl_end`/`rl_mark` globals, the readline
//! state bits, and the GNU-layout keymap arrays.  Before this module the
//! exported state was standalone: a loadable builtin could read and write a
//! private buffer, but the interactive Reedline editor never saw it.
//!
//! The two directions wired here are:
//!
//! * **editor → ABI** at every host boundary (`read_line` start, a `bind -x`
//!   command, a widget, an accepted line).  The live buffer/point are copied
//!   into `rl_line_buffer`/`rl_point`/`rl_end`, and `rl_readline_state`,
//!   `rl_done`, `rl_pending_input` are put in their boundary shape.
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

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::os::raw::{c_char, c_int, c_ulong};
use std::ptr;
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

/// Boundary entered just before the editor reads a line: reset the
/// one-shot state, mark the read command active, and mirror the buffer.
pub fn begin_read(buffer: &str, point: usize, prompt: &str) {
    sync_editor_to_abi(buffer, point, Some(0));
    deactivate_abi_mark();
    if let Ok(prompt) = CString::new(prompt) {
        // SAFETY: rl_set_prompt copies the NUL-terminated string.
        unsafe { readline::rl_set_prompt(prompt.as_ptr()) };
    }
    set_abi_pending_input(0);
    set_abi_done(0);
    remove_state(RL_STATE_DONE | RL_STATE_EOF | RL_STATE_INPUTPENDING);
    add_state(RL_STATE_READCMD);
}

/// Boundary after a line was accepted: the submitted text stays visible to a
/// command/plugin running after readline returns, exactly as GNU keeps the
/// accepted line in `rl_line_buffer` until the next `readline()` call.
pub fn finish_accepted(line: &str) {
    sync_editor_to_abi(line, line.len(), Some(0));
    set_abi_pending_input(0);
    remove_state(RL_STATE_READCMD | RL_STATE_DISPATCHING);
    add_state(RL_STATE_DONE);
    set_abi_done(1);
}

/// Boundary after Ctrl-C/Ctrl-D/eof: the line was abandoned.
pub fn finish_abandoned() {
    sync_editor_to_abi("", 0, Some(0));
    deactivate_abi_mark();
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

/// Walk one keymap trie and record every non-null function slot by its raw
/// key path.  Macro slots are not plugin functions and stay out; a macro
/// bind still shadows an older function entry, which the diff sees as a
/// removal.
fn collect_map(
    map: *mut readline::KeymapEntry,
    path: &mut Vec<u8>,
    out: &mut BTreeMap<(usize, Vec<u8>), usize>,
    root: usize,
    depth: usize,
) {
    if map.is_null() || depth > MAX_KEYMAP_DEPTH {
        return;
    }
    for key in 0..readline::KEYMAP_SIZE - 1 {
        let entry = RawEntry::read(map, key);
        if entry.kind == readline::ISFUNC {
            if entry.function != 0 {
                let mut full = path.clone();
                full.push(key as u8);
                out.insert((root, full), entry.function);
            }
        } else if entry.kind == readline::ISKMAP && entry.function != 0 {
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
    }
}

fn scan_maps() -> BTreeMap<(usize, Vec<u8>), usize> {
    let mut out = BTreeMap::new();
    for root in [ROOT_EMACS, ROOT_VI_INSERT, ROOT_VI_MOVEMENT] {
        let mut path = Vec::new();
        collect_map(root_map(root), &mut path, &mut out, root, 0);
    }
    out
}

/// Keymap snapshot keyed by `(root, key sequence)` -> bound function.
type KeymapSnapshot = BTreeMap<(usize, Vec<u8>), usize>;

static BASELINE: Mutex<Option<KeymapSnapshot>> = Mutex::new(None);
static INSTALLED_FINGERPRINT: Mutex<Option<u64>> = Mutex::new(None);

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

fn baseline() -> BTreeMap<(usize, Vec<u8>), usize> {
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

#[derive(Debug, Clone, Default)]
pub struct PluginState {
    pub fingerprint: u64,
    pub bindings: Vec<PluginKeyBinding>,
    pub unbinds: Vec<PluginKeyUnbind>,
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

/// Diff the live keymaps against the baseline and return the plugin-owned
/// bindings, removed slots, and a fingerprint.  The fingerprint is stable
/// across calls with no keymap movement, so the REPL can rebuild only when
/// a plugin actually registered or removed something.
pub fn plugin_state() -> PluginState {
    let baseline = baseline();
    let current = scan_maps();
    let mut keys: BTreeSet<(usize, Vec<u8>)> = BTreeSet::new();
    keys.extend(baseline.keys().cloned());
    keys.extend(current.keys().cloned());

    let mut state = PluginState::default();
    let mut hash = std::collections::hash_map::DefaultHasher::new();

    for (root, path) in keys {
        let before = baseline.get(&(root, path.clone())).copied();
        let after = current.get(&(root, path.clone())).copied();
        if before == after {
            continue;
        }
        (root, path.clone()).hash(&mut hash);
        after.hash(&mut hash);

        let Some(keys) = keys_from_path(&path) else {
            continue;
        };
        match after {
            Some(_) => state.bindings.push(PluginKeyBinding {
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
}
