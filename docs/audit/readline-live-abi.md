# Live readline ABI routing (niubash#775)

This note records the bidirectional state of the in-process GNU readline
C ABI after the `feat/readline-live-complete` slice (rebased onto `3018703`,
ruline `7a90663`), the remaining gaps, and the platform review that could
not be executed on this host.

Code: `crates/niubash-runtime/src/readline_live.rs`,
`crates/niubash-runtime/src/bind_surface.rs`,
`crates/niubash-runtime/src/shell.rs`,
`crates/niubash-runtime/src/repl.rs`.

## Now live in both directions

| Direction | Surface | Mechanism |
| --- | --- | --- |
| editor → ABI | `rl_line_buffer`, `rl_point`, `rl_end`, `rl_mark` | mirrored at every host boundary (`begin_read`, `bind -x`, widget, ABI callback, accepted line) |
| ABI → editor | buffer/point writes from a plugin | read back, UTF-8 clamped, applied with `EditCommand`; an active mark becomes a selection |
| plugin → editor | `rl_add_defun` / `rl_bind_key*` / `rl_unbind_*` | keymap diff against the pristine ABI defaults, rebuilt into the Reedline keymap between prompts |
| plugin → host | `rl_macro_bind` | collected from `ISMACR` slots and registered in the engine bind registry, so the ordinary engine-macro layer installs it and `bind -s` lists it |
| host → ABI | rc / inputrc `bind`, `bind -x`, macros, `bind -r` tombstones | `Shell::load_engine_bindings` mirrors them into the exported keymap arrays; only changed slots are rewritten, so a plugin bind on an untouched host key survives until the host rebinds it (GNU last-write) |
| plugin → editor | `rl_pending_input` | consumed at the next resume and replayed as the key (`rl_read_key`, input.c:786) |
| plugin → editor | `rl_done` | consumed at the next resume; the current buffer is submitted without an Enter keypress |
| plugin → editor | `rl_startup_hook` / `rl_pre_input_hook` | startup once before the first prompt, pre-input once per fresh `readline()` call (not on a host-command resume) |
| plugin → host | `rl_mark` | an active `rl_mark` survives a host-command resume (the resume boundary no longer resets it); only a fresh line resets it like `rl_initialize` |

`rl_function_of_keyseq`/`_len` consumers now see host `bind` functions
through the real `rl_named_function` pointer, `bind -x` through a host
command sentinel (non-null, distinct from the default command), and host
macros through `ISMACR` slots. Host-only bash command names
(`shell-expand-line`, `glob-complete-word`, …) are registered once with
`rl_add_defun` so `rl_named_function` agrees with the keymap slot.

## Remaining gaps

1. **Full `rl_pending_input` keymap dispatch.** Reedline exposes no public
   event-injection entry point, so the host replays the common default-map
   keys as `EditCommand`s (`apply_pending_key`): self-insert, C-a/C-e/C-b/
   C-f, C-d, C-k/C-u/C-w, C-y, C-t, C-_, Backspace, Tab (complete), C-@
   (set-mark) and Enter (accept). A key bound by a plugin (or a vi
   motion) reaches the editor only if it is in that table.
2. **Host-only funmap identity.** Every host command the ABI funmap does
   not know materializes as one process-wide `host_command_sentinel`
   pointer. `rl_function_of_keyseq` is non-null for each, but two such
   names compare equal. GNU gives each its own bash function pointer.
3. **Plugin function binds are not mirrored back into `bind -p`.** Only
   plugin macros join the engine registry today; the editor keymap layer
   consumes plugin functions directly.
4. **`-m emacs-meta` / `emacs-ctlx` host entries** are mirrored relative to
   the emacs-standard root, matching the host's own keymap policy
   (`engine_keymap_applies`), not GNU's literal submap targeting.
5. **A plugin macro unbound later** stays in the engine registry (the
   editor keymap hides it via the plugin-unbind layer, but `bind -s` can
   still list it).
6. **Mark granularity.** Reedline selections are byte offsets; a mark that
   is not a selection anchor is preserved as the exported number only.

## Proposed (not applied) Reedline API patch

The clean fix for gap 1 is a public injection entry point in the pinned
Reedline (`src/engine.rs`). Its private queues already do the work:

```rust
impl Reedline {
    /// Queue host-supplied events ahead of live terminal input, for a
    /// readline host that must inject the next key (`rl_pending_input`)
    /// or replay a macro (`ReedlineEvent::KeySequence`).
    pub fn queue_events(&mut self, events: Vec<ReedlineEvent>) {
        self.pending_events.extend(events.into_iter().map(|event| (event, 0)));
    }
}
```

The host would then call `queue_events(vec![key_event])` instead of the
`EditCommand` table in `apply_pending_key`, giving exact GNU dispatch
through the live keymap (including plugin and vi bindings). This is a
`ruline` change; the ruline checkout is not edited by this branch.

## Windows/MSVC and Mach-O review (code-path only)

No Windows or Mach-O binary was built or run on this Android/Termux host.
Static review of the new code paths:

- **No platform-specific calls.** `readline_live.rs` uses only exported
  `readline` symbols, `CStr`/`CString`, `Mutex`, `AtomicBool`, and
  `std::os::raw` types. There is no `#[cfg(unix)]`, `libc`, `dlsym`,
  `ioctl`, or tty access in the new code.
- **c_char width.** Key paths are `Vec<u8>`; the one `c_char` buffer is
  `[0 as c_char; 256]` for `rl_translate_keyseq`, and results are
  converted with `as u8`. `c_char = i8` (MSVC/MinGW) and `u8` (aarch64
  Linux) both round-trip; `octal_spelling` emits only ASCII.
- **Export gate.** No new dynamic symbols are required: the mirror calls
  `rl_bind_keyseq_in_map`, `rl_macro_bind`, `rl_named_function`,
  `rl_add_defun`, `rl_translate_keyseq`, `rl_untranslate_keyseq`,
  `rl_function_of_keyseq[_len]`, `rl_set_mark`, `rl_activate_mark`, and
  the exported hook/keymap globals — all already in
  `scripts/readline-abi-exports.txt`. `build.rs`'s ELF `-rdynamic`,
  Mach-O `-export_dynamic`, MSVC `.def` and MinGW export-all paths are
  unchanged and keep covering them.
- **Callbacks.** `rl_startup_hook` / `rl_pre_input_hook` are read from the
  exported data symbols and invoked with the C ABI on the main thread;
  the calling convention matches the ABI crate's `RlHookFunc` on all
  supported targets.
- **Probe harness.** `scripts/readline-plugin-probes/loadable_builtin.c`
  resolves symbols with `dlsym(RTLD_DEFAULT)`, which is POSIX-only. A
  Windows lane would need a `GetProcAddress(GetModuleHandle(NULL), …)`
  shim and `.def`-exported `rl_probe_struct`; `pty_probes.py` needs a
  ConPTY driver. Both are out of scope for this host and remain a matrix
  SKIP rather than a claimed pass.
- **Mach-O.** `enable -f` style `dlopen` loading works on macOS; the
  exported-symbol gate is the existing `-export_dynamic` path. No code in
  this slice assumes ELF symbol interposition.

## Verification on this host

- `scripts/readline-plugin-probes/run.sh`: 11/11 PASS, including the
  extended `loadable-builtin-niu` probe (host bind/bind -x visibility,
  plugin macro round-trip, pending-input injection, `rl_done` submit,
  mark across resume, startup=1 / pre-input per line).
- `scripts/readline-keyboard-oracle/run.sh`: 50/50 PASS, 0 FAIL, 0 KNOWN
  (the previously documented `complete/default_style_tab` KNOWN now passes
  on the rebased master).
- `cargo test -p niubash-runtime --lib readline_live::`: 10 tests.
