---
tags: [winuxsh, v2.2, reference, nushell, audit]
created: 2026-07-17
status: active
---

# Nushell Reference Audit for Winuxsh v2.2

> Purpose: use Nushell as a modern shell/frontend reference, not as a dependency
> or language model. Winuxsh remains: rubash shell semantics + winuxcmd.exe PATH
> injection + reedline REPL.

## Reference Snapshot

- Reference repository: `https://github.com/nushell/nushell`
- Local reference clone: `%TEMP%/winuxsh-reference/nushell`
- Observed commit: `c1675b7`
- Read scope:
  - `crates/nu-cli/src/repl.rs`
  - `crates/nu-cli/src/reedline_config.rs`
  - `crates/nu-cli/src/completions/*.rs`
  - `crates/nu-config/default_files/default_config.nu`
  - `crates/nu-protocol/src/config/*.rs`

## Architecture Takeaways

### 1. REPL frontend should stay compositional

Nushell builds the line editor by composing history, completer, hints, menus, and
edit mode around reedline. This maps well to `winuxsh-runtime/src/repl.rs`,
which already composes `FileBackedHistory`, `WinuxshCompleter`, and `ListMenu`.

For winuxsh v2.2, keep this small:

- Add config-driven edit mode: `emacs` default, `vi` optional.
- Add explicit history search menu / keybinding only if reedline defaults are not enough.
- Keep history path as `~/.winuxsh_history` for compatibility.
- Do not change rubash execution semantics from the REPL layer.

### 2. Completion should use provider stages, not one giant completer

Nushell separates command, flag, flag-value, path, variable, and custom
completion. Winuxsh already has a compatible direction through
`CompletionPlugin`, `CommandCompletionPlugin`, `ExternalCompletionPlugin`,
`PathCompleter`, and `VariableCompleter`.

For winuxsh v2.2, the right next feature is not a rewrite; it is to strengthen
the current provider model:

- Fix the current completion integration test drift first.
- Add bundled default TOML definitions for high-frequency winuxcmd commands.
- Preserve user completion directories as overrides/extensions.
- Add value completions only where the value domain is stable.
- Postpone fuzzy/substring matching until basic bundled definitions are green.

### 3. Config should grow by stable sections

Nushell keeps separate config sections for completions, edit mode, history,
keybindings, menus, and cursor shape. Winuxsh should not copy that full schema,
but the separation is useful.

Recommended v2.2 config direction:

- Keep structured plugin state in manifest-backed registry files.
- Add a minimal `[editor]` section for `edit_mode = "emacs" | "vi"`.
- Add a minimal `[history]` section only after Ctrl+R/search behavior needs user
  controls.
- Keep `[completions]` as the completion definition entry point.
- Make the existing `[winuxcmd].path` field actually affect PATH injection.

### 4. Windows command discovery must avoid PowerShell semantics

Nushell is Windows-aware when discovering external commands. That is useful for
completion UX, but winuxsh must not become PowerShell-like. Command execution
continues to flow through rubash PATH lookup and winuxcmd shims.

Practical rule:

- Completion may be Windows-aware about executable suffixes.
- Execution must remain Bash-compatible and should not adopt PowerShell wildcard,
  quoting, or pipeline semantics.
- Do not add `.ps1` behavior unless explicitly scoped as a completion-only
  convenience and reviewed separately.

## What Maps to Winuxsh Now

- Bundled default completion definitions for `ls`, `grep`, and `find`.
- Config-driven reedline edit mode.
- Ctrl+R history search/menu through reedline.
- Small config schema additions, not a large config rewrite.
- Documented provider order for completion.

## What Is Postponed

- Fuzzy/substring completion matching.
- Custom keybinding DSL.
- SQLite/history isolation.
- External completion command bridge.
- Nushell-style command metadata system.
- Plugin ABI design.

## What Is Out of Scope

- Adding Nushell crates to `Cargo.toml`.
- Vendoring Nushell source into this repository.
- Replacing rubash lexer/parser/executor.
- Reintroducing winuxcmd FFI/DLL.
- Adopting Nushell structured pipelines or syntax.
- Treating PowerShell as the target shell behavior.

## Immediate v2.2 Plan

1. Fix completion integration test drift:
   - `crates/winuxsh-runtime/tests/completion.rs` calls `load_completion_dir`.
   - Current implementation exposes `load_completion_dirs`.
2. Add a bundled default completion directory and first definitions:
   - `ls`
   - `grep`
   - `find`
3. Source flags from installed `winuxcmd.exe <cmd> --help` output or a pinned
   WinuxCmd source/tag; do not guess.
4. Add tests proving:
   - bundled definitions load without user config
   - user completion dirs still work
   - existing TOML fixture behavior remains intact
5. After completion foundation is green, implement `[editor].edit_mode` and
   Ctrl+R history search wiring in `repl.rs`.

## Decision on Pulling Nushell Source

Pulling Nushell source is useful as temporary reference material because it
shows a mature reedline shell frontend, configurable edit modes, history menus,
and staged completion providers. It should stay outside the winuxsh repository
under a temp/reference directory, and the audited commit should be recorded.

It is not useful to vendor Nushell or add it as a dependency for v2.2. The source
is a design reference only; the implementation should stay native to winuxsh's
existing rubash + winuxcmd + reedline architecture.
