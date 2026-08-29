---
tags: [niubash, roadmap, v2.2, reference, nushell]
created: 2026-07-17
status: active
---

# Niubash v2.2 Nushell Reference Plan

> Goal: before changing runtime behavior, use modern Windows shell references to keep
> niubash aligned with its target experience: Bash-compatible workflow on Windows,
> powered by rubash + winuxcmd + reedline.

## Reference Scope

- Nushell is a **reference source only**. Do not add Nushell crates to niubash and do not vendor downloaded source into this repository.
- Current Nushell reference snapshot: `%TEMP%/niubash-reference/nushell`, commit `c1675b7`.
- Clone/read Nushell outside the repository to study how a modern Windows-friendly shell structures:
  - completion metadata and argument/value completion
  - command discovery and PATH integration
  - history search and edit modes
  - theming / prompt customization
  - config loading and compatibility migration
- Treat Nushell as UX/architecture reference, not shell-language reference. Niubash targets Bash-compatible behavior via rubash, not Nushell syntax or pipeline semantics.
- Keep niubash's locked architecture:
  - shell language semantics stay in rubash
  - coreutils stay as winuxcmd.exe through PATH injection
  - REPL/frontend behavior stays in reedline
  - no winuxcmd FFI/DLL reintroduction
  - no PowerShell wildcard semantics in niubash command execution

## Candidate References

### Nushell

- Repository: `https://github.com/nushell/nushell`
- Reference use:
  - REPL loop boundaries and engine/frontend separation
  - completion provider organization
  - config file layout and migration approach
  - history/search UX and edit-mode settings
  - prompt/theme conventions
- Out of scope:
  - Nushell language semantics
  - structured data pipeline model
  - plugin ABI/runtime unless planning v3 plugin framework
  - replacing rubash or reedline

### Existing local/runtime references

- `reedline 0.33.0`
  - Reference use: direct implementation source for Vi mode and Ctrl+R history search.
  - Current finding: default common keybindings already map Ctrl+R to `SearchHistory`; Vi uses `Vi::new(default_vi_insert_keybindings(), default_vi_normal_keybindings())`.
- `winuxcmd.exe 0.12.0`
  - Reference use: command list and command-specific `--help` output for default completion definitions.
  - Current finding: installed via Winget portable zip.
- `rubash` pinned rev `f08d6d68e4901332c0003be5339f9f80f6251ae2`
  - Reference use: shell behavior boundary only. Do not duplicate lexer/parser/executor logic in niubash.

## Current Guardrails Before Feature Work

- Fix completion integration test drift:
  - `crates/niubash-runtime/tests/completion.rs` still calls `load_completion_dir`.
  - implementation now exposes `load_completion_dirs`.
  - Confirmed by `cargo test -p niubash-runtime --test completion --locked` on 2026-07-17.
- Re-run:
  - `cargo fmt --check`
  - `cargo test --lib -p niubash-runtime --locked`
  - `cargo test -p niubash-runtime --test completion --locked`
  - `cargo test --test compat -- --ignored` when winuxcmd is available

## v2.2 Execution Plan

### Phase 0 - Preflight / Hygiene

- [x] Removed the empty untracked `--help` directory created by accidental PowerShell alias probing.
- [x] Kept `.tmp/` untracked and out of commits.
- [x] Confirmed current local branch state is `master...origin/master [ahead 2]` before v2.2 implementation work.

### Phase 0 - Reference Audit

- [x] Clone/read Nushell source outside the repository.
- [x] Summarize relevant patterns into Obsidian/repo docs:
  - completion model
  - history/editing model
  - prompt/theme model
  - config/profile model
- [x] Decide what maps cleanly to niubash and what is explicitly out of scope.
- Audit note: `nushell-reference-audit.md`

### Phase 1 - Completion Test Baseline

- [x] Fix the stale completion integration test.

### Phase 2 - Built-in Completion Foundation

- [x] Add bundled default completion definitions for a small first batch:
  - `ls`
  - `grep`
  - `find`
- [x] Add tests proving bundled definitions load without user config and user TOML dirs still work.
- [x] Keep definitions derived from `winuxcmd.exe <cmd> --help` output or pinned WinuxCmd source/tag.
- Verification: `cargo fmt --check`, `cargo test --lib -p niubash-runtime --locked`, and `cargo test -p niubash-runtime --test completion --locked` passed.

### Phase 3 - Completion Expansion

- [x] Expand default definitions after the foundation is green:
  - `cat`, `cp`, `mv`, `rm`, `mkdir`, `touch`, `chmod`
- [ ] Treat `tar` separately because behavior may come from bundled bsdtar/system tar; verify implementation before defining options.
- [x] Add value completions only where the value domain is clear and stable.
- Verification: `cargo fmt --check`, `cargo test --lib -p niubash-runtime --locked`, and `cargo test -p niubash-runtime --test completion --locked` passed.

### Phase 4 - Reedline UX

- [x] Add config fields for editor mode:
  - default `emacs`
  - optional `vi`
- [x] Wire reedline edit mode in `repl.rs`.
- [x] Confirm Ctrl+R remains active in both modes through reedline's common keybindings.
- Verification: cargo fmt --check, cargo test --lib -p niubash-runtime --locked, and cargo test -p niubash-runtime --test completion --locked passed.

### Phase 5 - Config Consistency

- [x] Honor `[winuxcmd].path` during PATH injection.
- [x] Keep `winuxcmd.exe` integrated through PATH only, with no FFI/DLL path.
- [x] Re-run `cargo build --locked` and compat tests because shell startup/PATH changes are affected.
- Verification: `cargo fmt --check`, `cargo test --lib -p niubash-runtime --locked`, `cargo test -p niubash-runtime --test completion --locked`, `cargo build --locked`, and `cargo test --test compat -- --ignored` passed.

### Phase 6 - User Themes

- [x] Design a TOML theme schema under `~/.niubash/themes/*.toml`.
- [x] Add loader that falls back to existing built-in themes.
- [x] Keep built-in themes stable for compatibility.
- [x] Cover only the current `theme.rs` style surface: prompt user/host/dir/symbol, error, warning, success.
- Verification: `cargo fmt --check`, `cargo test --lib -p niubash-runtime --locked`, `cargo test -p niubash-runtime --test completion --locked`, `cargo build --locked`, and `cargo test --test compat -- --ignored` passed.

## Obsidian Workflow

- Maintain `niubash/` folder in the vault as the active project memory area.
- Before feature implementation, update the relevant Markdown plan/checklist.
- After each phase, sync:
  - `../src/niubash-roadmap.md`
  - `niubash-v2.2-reference-plan.md`
  - Obsidian `niubash/` copies
