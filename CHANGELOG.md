# Changelog

All notable changes to Niubash are documented in this file.

## [1.1.5] - 2026-09-25

### Fixes

- **Drive-letter colons are preserved when niu splits the shell `PATH`**
  (`3c7b4b1`); PATH entries like `C:/tools/bin` no longer get mangled into
  `C` + `/tools/bin` during PATH processing
- GNU-aligned invocation surface for stdin scripts: fd0 handling and `-i`
  history flag match GNU bash behavior (`746b74d`, rubash-side fixes)
- Engine bump: niu 1.1.5 builds against rubash 1.2.0 (published to crates.io),
  which carries the `/dev/stdout` `/dev/stdin` `/dev/null` redirect semantics
  fixes (`8c0dded3`..`a38268f6`) and the gate suite now runs 86/86 green

## [1.1.3] - 2026-09-16

### Fixes

- **`ln -s` with `./` / `../` targets produced links that Explorer could not open**.
  winuxcmd stored the link text verbatim; NT only resolves reparse-point targets
  with backslash separators, so any forward slash (`./bds`, `../x`, `dir/file`)
  failed native resolution with "The filename, directory name, or volume label
  syntax is incorrect" (WinuxCmd #1101, fixed in v1.0.8, niubash #109)
- **`niu -lc 'cmd'` (and any bundled short option containing `-l`/`-i`) failed**
  with `-l: invalid option`; bundled short options now expand correctly
  (`-lc`, `-cl`, `-ilc`, `-ic`) (rubash, PR #112)
- `niu -c -l`, `niu -c` argument handling and `$(type -t)` stdout leak from
  the v1.1.2 issue batch (#106/#107/#108, rubash PR #111)

## [1.1.0] - 2026-09-12

### Highlights

One week of intensive work after v1.0.1: IDE-style completion menu, colorized
plugin CLI, NIU_ENV/BASH_ENV one-shot env files, startup-overhead fix, release
binary size reduction (LTO + panic=abort), and upstream rubash v1.1.0 with the
CTLESC `\x11` leak fix that resolves `NIU_BINDKEYS` parsing.

### Features

- **IDE-style completion menu** with descriptions; humanized plugin CLI views
  (`26610e7`, `a6c1fee`)
- **Colorized plugin CLI**: TTY-gated ANSI styling for plugin list/action
  feedback (`cb72d52`, `3ab1b42`)
- **NIU_ENV / BASH_ENV opt-in env files** for one-shot mode (#82, `ee1c410`)
- **Interactive shell easter eggs** (`30efd9a`)
- **Demo bundle** for plugin development (`f7d2943`)

### Fixes

- **Startup overhead**: skip framework hook dispatch when the runner is
  undefined (#80, `6fbee9f`)
- **CTLESC `\x11` leak** (upstream rubash v1.1.0): quoted assignment fast path
  leaked `\x11` into stored values, breaking `NIU_BINDKEYS="Ctrl+X:..."` parsing
  (`a313729b` in rubash)
- **Hook runner resilience**: hook runners survive user `set -eu` (`6ba7af0`)
- **REPL heredoc-body scanning**: fix completeness check for heredoc bodies
  spanning command-substitution boundaries (`6ba7af0`)
- **WinuxCmd auto-activation**: portable first-run activation with absolute-path
  activate script (`ca574ed`)
- **Panic hook**: best-effort console restore under `panic=abort` (`270d16b`)
- **Rename brand gate**: restore working version drift gate (`92e6724`)
- **$BASH path**: set `$BASH` to the running executable path (`0ea5513`)
- **Completion engine**: wire rubash completion engine, drop hardcoded builtin
  list (`316a8f6`)
- **Heredoc diagnostics** (upstream rubash v1.1.0): warning line numbers now
  use computed `warning_line` matching GNU `make_cmd.c:627` (`a313729b` in
  rubash; remaining gaps tracked in rubash issue #72)
- **Chinese path crash** (#84): `host_path_to_shell_path_with_root` panicked
  with `byte index is not a char boundary` when the shell root byte length
  fell inside a multi-byte character in the current directory path. Added
  `is_char_boundary` guard before slicing. Also fixed
  `longest_common_prefix` in the completion module which had the same class
  of bug when decrementing `prefix_len` through multi-byte characters.

### Build

- **Release binary size**: enable thin LTO + `panic=abort` (-35% size,
  `f94c48d`)
- **Warning suppression**: crate-level `#![allow]` for 11 legacy rubash warnings
  to unblock downstream CI (`a313729b` in rubash)

### Documentation

- Locale default decision for `${#var}` UTF-8 counting (`2f86b3e`)
- Trim redundant README sections; drop stale arm64/version claims (`ceb431e`)
- Add Built-ins & Fast Paths page (`1e35971`)
- Translate architecture page to English (`5a02b7b`)

### Dependencies

- Rubash upgraded from v1.0.0 to **v1.1.0**

## [1.0.1] - 2026-09-04

Initial stable release.
