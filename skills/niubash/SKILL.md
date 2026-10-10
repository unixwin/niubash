---
name: niubash
description: Run Windows tasks in Niubash, the GNU Bash-compatible Windows-native shell. Use when the active shell is Niubash (the `niu` prompt), when editing ~/.niubashrc or oh-my-niu plugins, when installing Unix commands with wpm, or for any Windows task where bash syntax should run natively — no PowerShell, WSL, Git Bash, or MSYS involved, and no path-conversion environment variables required.
---

# Niubash

## Identity — a bash dialect, not a POSIX machine

Niubash is bash implemented natively in Rust for Windows: the language
engine ([rubash](https://github.com/unixwin/rubash)) is a from-scratch Bash
interpreter gated on GNU Bash's own upstream test suite — **82 of 83 full
suites byte-identical to GNU output** (2026-09-25 ledger re-run; the one
residual, nameref, is individually audited — ledger in rubash's
[README, "Compatibility at a
Glance"](https://github.com/unixwin/rubash#compatibility-at-a-glance)). What it is not matters as much:

- **No POSIX emulation layer.** No `cygwin1.dll`/`msys-2.0.dll`, no fork,
  no POSIX signal delivery, **no PTY emulation** — job control and terminal
  handling ride on Win32/ConPTY. Interactive fullscreen TUI programs built
  for a POSIX pty (and pty-only tools like `script`/`ssh -t`) are the known
  rough edge, not path or quoting behavior.
- **No MSYS path rules.** The Windows-native path is the first-class
  internal representation; `/c/...`, `/mnt/c/...`, `/usr/bin`, `/tmp` are
  input dialects that resolve to real Windows locations. There is no
  conversion heuristic to misfire, so `MSYS_NO_PATHCONV`-style variables do
  not exist here and are never needed. Full contract:
  `references/paths.md`.
- **Verbatim argv at the boundary** ("Option B"): the bundled `bash.exe` /
  `sh.exe` forwarders pass the command line through to `niu.exe` untouched
  — no re-quoting, no path rewriting at the shim. What a child process
  receives is always a native Windows path when the argument is a shell
  path; literal data arguments (`printf '%s\n' /tmp/x`) are never rewritten
  (GNU bash behavior).
- **No bundler downloads.** Plugin sources arrive by git clone only; fonts
  and CLI tools are package-manager recommendations. `wpm`/WinuxCmd is the
  separate command-layer package manager.

## Core rules

- The current session already **is** Niubash. Run commands directly; do not
  nest `niu -c`, and do not route work through `pwsh`, `powershell`,
  `cmd /c`, `wsl`, or `bash` wrappers.
- Write bash syntax normally: functions, arrays, `$(...)`, pipes, heredocs,
  globs. For deep bash-isms (`coproc`, exotic redirects, `mapfile` edge
  cases, `compgen`/`complete`), test the exact form in-session before
  relying on it — then trust the measured floor.
- Discover, don't assume. Before using a tool: `command -v <tool>`. For the
  installation as a whole: `niu doctor`. For the command runtime:
  `winuxcmd --version`. For installed packages: `wpm installed`.
- A Unix command is missing? Install it with `wpm install <name>` — see
  `references/wpm.md`.

## Capability surface

<!-- BEGIN GENERATED:capability-snapshot -->
- **77 shell built-ins & reserved words** — the GNU bash 5.3 `shell_builtins[]` table (`alias`, `cd`, `declare`, `printf`, `set`, `trap`, `if`/`while`/`case`, ...); every one documents itself via `help <name>`.
- **180 winuxcmd applets** on PATH (`ls`, `grep`, `sed`, `awk`, `find`, `tar`, `top`, `xxd`, ...) — real Windows binaries, not emulated inside the shell; full inventory in `references/quickref.md`.
- **Launcher verbs** — `setup`, `font`, `doctor`, `plugin`, `skill`, `--self-update`; table in `references/quickref.md`.
- **Plugin system** — git-clone-only external sources (oh-my-bash, bash-it, bpkg) behind an explicit trust gate, driven by a declarative spec (`~/.niubash/plugins.toml`) with lockfile verbs (`add/list/enable/disable/trust/sync/update/restore/rollback/clean/source/recipe/distro/mirror/ui`).
- **TUI surfaces** — `niu setup` wizard (theme/prompt/tools), `niu plugin ui` menu, theme gallery, `niu font` picker; all non-interactive-friendly and safe to skip (`-c` loads none of them).
- **Bundle version**: niubash 1.3.4 (generated; see `scripts/generate-skill.py`).
<!-- END GENERATED:capability-snapshot -->

Shell built-ins (the generated list below is the engine's own GNU bash 5.3
`shell_builtins[]` table — reserved words included):

<!-- BEGIN GENERATED:builtin-names -->
`!` `%` `(( ... ))` `.` `:` `[` `[[ ... ]]` `alias` `bg` `bind` `break` `builtin` `caller`
`case` `cd` `command` `compgen` `complete` `compopt` `continue` `coproc` `declare` `dirs`
`disown` `echo` `enable` `eval` `exec` `exit` `export` `false` `fc` `fg` `for` `for ((`
`function` `getopts` `hash` `help` `history` `if` `jobs` `kill` `let` `local` `logout`
`mapfile` `popd` `printf` `pushd` `pwd` `read` `readarray` `readonly` `return` `select`
`set` `shift` `shopt` `source` `suspend` `test` `time` `times` `trap` `true` `type`
`typeset` `ulimit` `umask` `unalias` `unset` `until` `variables` `wait` `while` `{ ... }`
<!-- END GENERATED:builtin-names -->

Every built-in documents itself: `help <name>` (`help -s '*'` for the one
line per command). Launcher verbs and the plugin verb table:
`references/quickref.md`.

## Common pitfalls

- **Path forms.** Native `C:/Users/me` (or `C:\Users\me`) is always safe
  and is what Windows executables should receive. `/tmp` is the **real
  Windows temp directory**, never the install tree; `/usr/bin` is the
  WinuxCmd install root, not a POSIX root; `~` = `USERPROFILE`. Details and
  the MSYS contrast table: `references/paths.md`.
- **Theme system.** The interactive prompt is configured by `~/.niubashrc`
  (`NIU_THEME`, `NIU_PLUGINS=(...)` packs) and `niu setup`; icon themes
  need a Nerd Font (`niu font`). Don't invent theme or pack names — check
  `references/prompt-plugins.md`, and never edit files inside the installed
  bundle directory (user plugins live in `~/.niubash/custom`).
- **Startup flow.** Interactive start sources `~/.niubashrc`, which loads
  the oh-my-niu bundle and ends in a guarded `plugin sync --bootstrap`
  block (idempotent, keyed on `"${NIU_SHELL:-niu}"`). `niu -c` and script
  runs load **no rc and no plugins** by design — quiet and deterministic.
  Non-interactive init: `NIU_ENV=<file>` (or bash-compatible `BASH_ENV`,
  lower precedence) is sourced once before the command.
- **When `NIU_SHELL` matters.** Niubash exports it (its own executable
  path) in every process it starts; the `bash`/`sh` forwarder shims and the
  rc bootstrap resolve the shell through it. When sourcing `~/.niubashrc`
  from a *foreign* shell, the guarded block is safe only because of that
  fallback chain — don't "simplify" it to a bare `niu`.

## Decision guidance — niu vs cmd vs powershell

| Task | Use |
| --- | --- |
| Anything with bash syntax (loops, pipes, substitution, globs, scripts) | **niu** — directly in-session |
| Running Windows-native programs (`git.exe`, `node.exe`, `cargo.exe`) | **niu** — your PATH is your PATH, no conversion step |
| `cmd`-specific quirks (batch files needing `cmd` semantics, `reg` legacy forms) | `cmd /c ...` from niu, but prefer native equivalents |
| .NET/COM, Windows management, registry scripting, remoting | **powershell** — call it from niu when needed |
| Linux-only tooling, systemd, Linux binaries | **WSL** — niubash does not emulate Linux |
| POSIX-pty interactive programs (expect/tmux-style) | Windows-native equivalents; no pty emulation here |

Rule of thumb: if the task is "run a Unix command or a bash script on
Windows", that is exactly this shell. Reach for another shell only for its
own native APIs, never as a compatibility detour.

## Validation

- Verify in-session: run the snippet directly (or source a temporary file)
  before editing rc files; then confirm with `command -v`, `alias`, or
  `test -f C:/path`.
- Validate installs with direct calls: `rg --version`, `wpm links list`.
- On a bash-semantics divergence: fall back to a simpler POSIX form, then
  report the gap upstream to `unixwin/rubash`. Do not carry host-side
  workarounds.
- Reserve `niu -c "..."` for when an external host must launch Niubash
  non-interactively: quiet, deterministic, exit codes propagate exactly.

## References

- `references/quickref.md` — generated command tables: launcher verbs,
  plugin verbs, full built-in table, winuxcmd applet inventory.
- `references/paths.md` — the path contract and the MSYS contrast.
- `references/prompt-plugins.md` — rc contract, themes, oh-my-niu packs.
- `references/wpm.md` — package discovery, links repair, index flows.
