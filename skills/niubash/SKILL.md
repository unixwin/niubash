---
name: niubash
description: Run Windows tasks in Niubash, the GNU Bash-compatible Windows-native shell. Use when the active shell is Niubash (the `niu` prompt), when editing ~/.niubashrc or the external plugin manifest, when installing Unix commands with wpm, or for any Windows task where bash syntax should run natively — no PowerShell, WSL, Git Bash, or MSYS involved, and no path-conversion environment variables required.
---

# Niubash

## Identity — a bash dialect, not a POSIX machine

Niubash is bash implemented natively in Rust for Windows: the language
engine ([rubash](https://github.com/unixwin/rubash)) is a from-scratch Bash
interpreter gated on GNU Bash's own upstream test suite — **all 83 suites
pass on the 2026-10-12 full re-run: 62 byte-identical stdout on Windows,
20 environment-attributed, and the nameref suite's single residual line
proven to be GNU Bash's own coproc reap-timing race** (ledger in rubash's
[README, "Compatibility at a
Glance"](https://github.com/unixwin/rubash#compatibility-at-a-glance)). What it is not matters as much:

- **No POSIX emulation layer on Windows.** The Windows stack carries no
  `cygwin1.dll`/`msys-2.0.dll`, no fork, no POSIX signal delivery, **no PTY
  emulation** — job control and terminal handling ride on Win32/ConPTY. Interactive fullscreen TUI programs built
  for a POSIX pty (and pty-only tools like `script`/`ssh -t`) are the known
  rough edge, not path or quoting behavior.
- **No MSYS path rules.** The Windows-native path is the first-class
  internal representation; `/c/...`, `/mnt/c/...`, `/usr/bin`, `/tmp` are
  input dialects that resolve to real Windows locations. Translation at
  the boundary is one deterministic contract, not a heuristic: classified
  POSIX spellings are translated for native children, while data-shaped
  tokens, the bare `/`, and drive-root forms go verbatim — so
  `MSYS_NO_PATHCONV`-style variables do not exist here and are never
  needed. Full contract: `references/paths.md`.
- **Verbatim argv at the boundary** ("Option B"): the bundled `bash.exe` /
  `sh.exe` are hardlinks to `niu.exe` — the same binary taking the GNU
  face from `argv[0]` (a forwarding shim stands in only where the
  filesystem cannot hardlink). No re-quoting at the boundary. What a
  native child process receives for a classified shell-path spelling is
  the native Windows path; literal data arguments
  (`printf '%s\n' /tmp/x`), the bare `/`, and drive-root forms (`/d`,
  `/d/`) are never rewritten.
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
- **178 winuxcmd applets** on PATH (`ls`, `grep`, `sed`, `awk`, `find`, `tar`, `top`, `xxd`, ...) — 176 commands plus the internal `wpm` package manager and `[`; real Windows binaries, not emulated inside the shell; full inventory in `references/quickref.md`.
- **Launcher verbs** — `setup`, `font`, `doctor`, `config`, `plugin`, `skill`, `--self-update`; table in `references/quickref.md`.
- **Verbatim handoff** — `--encoded-command`/`-EncodedCommand`/`--ec <b64>` runs a base64 payload with zero quoting; optional `v1:` prefix guards MSYS-family rewriting in transit.
- **Plugin system** — git-clone-only external sources (oh-my-bash, bash-it, bpkg), driven by a declarative spec (`~/.niubash/plugins.json`) with lockfile verbs (`add/list/enable/disable/sync/update/restore/rollback/clean/migrate-rc/source/recipe/distro/mirror/ui`). An installed source contributes once it is enabled — there is no trust state.
- **TUI surfaces** — `niu setup` wizard (theme/prompt/tools), `niu plugin ui` menu, theme gallery, `niu font` picker; all non-interactive-friendly and safe to skip (`-c` loads none of them).
- **Bundle version**: niubash 1.4.4 (generated; see `scripts/generate-skill.py`).
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

## Debugging scripts (new in niubash 1.5.0)

The debugger front ends below ship with niubash **1.5.0**; they are not
in 1.4.4 (this bundle's version). On 1.4.4, `--debug` keeps the legacy
extdebug-arming behavior described in the last bullet, and `--debugger`
is the GNU bashdb starter path.

- **`niu --debug script.sh [args…]`** starts the native interactive debugger
  stopped at the entry stop. The prompt is `(niu-dbg)`; the M1 command set
  includes `break`, `continue`, `next`/`step`, `where`, `frame`, `list`,
  `print`/`eval`, `set var`, `info …`, and `quit`/`kill`. Commands come from
  the controlling terminal, never the debuggee's stdin, so piped stdin is
  not a batch driver — use `source <file>` inside the session.
- **`niu --dap script.sh`** serves the same session over the Debug Adapter
  Protocol on stdio. `niu --dap` without a script is a pure server and the
  client's `launch` request names the program; `niu --debug --dap script.sh`
  is the spelled-out editor form. The bundled zero-JS VS Code extension is
  in `editors/vscode/` (type `niubash`, program `niu`).
- `--debug` with `-c`, `-s`, or `-i` keeps the legacy extdebug-arming
  behavior (no debugger frontend); `--debugger` remains the GNU bashdb
  starter path.

## Common pitfalls

- **Path forms.** Native `C:/Users/me` (or `C:\Users\me`) is always safe
  and is what Windows executables should receive. `/tmp` is the **real
  Windows temp directory**, never the install tree; `/usr/bin` is the
  WinuxCmd install root, not a POSIX root; `~` = `USERPROFILE`. Bare drive
  designators work the cmd way: `D:` and `cd D:` jump to that drive's
  root (`/d/`), identical across `-c`, brace groups, and piped `niu -i`.
  Details and the MSYS contrast table: `references/paths.md`.
- **Prompt & plugins.** There is no built-in theme engine: the rc is
  plain bash and the prompt belongs to whatever claims `PS1` — a hand-set
  line, `eval "$(starship init bash)"`, or an external framework
  (oh-my-bash, bash-it) installed through `niu plugin` and activated by
  the generated loader. `NIU_THEME`/`NIU_PLUGINS` lines in an old rc are
  inert assignments — the native pack/theme stack is retired
  (niubash#145). Icon themes need a Nerd Font (`niu font`). Contract:
  `references/prompt-plugins.md`.
- **Startup flow.** Interactive start sources the generated plugin loader
  `~/.niubash/generated/plugins.niu` — host-owned, derived from the
  `plugins.json` manifest, regenerated by `niu plugin` verbs, never
  hand-edited — BEFORE `~/.niubashrc`, so user PS1/alias/function
  overrides win. The loader stays off while the rc still carries a
  superseded managed block (a one-time hint names `niu plugin migrate-rc`,
  the read-only review; removing the block is always the user's own edit)
  or when `NIU_PLUGIN_LOADER=off` is set. No verb writes `~/.niubashrc`.
  `niu -c` and script runs load **no rc and no plugins** by design — quiet
  and deterministic. Non-interactive init: `NIU_ENV=<file>` (or
  bash-compatible `BASH_ENV`, lower precedence) is sourced once before
  the command.
- **When `NIU_SHELL` matters.** Niubash exports `NIU_SHELL` with its own
  `niu.exe` path in every process it starts, and fills `SHELL` — the
  POSIX-convention variable agents probe — with the same path when the
  host left it unset; the `bash`/`sh` faces and the rc bootstrap resolve
  the shell through `NIU_SHELL`. The pre-rename bridge name is retired
  and read by nothing — do not build on it.

## Behavior notes — recent engine fixes an agent can rely on

- A `wpm install` (or any new file landing on `PATH`) is runnable in the
  same session: the command resolver's negative cache invalidates when the
  file appears — no shell restart or `hash -r` needed (the builtin still
  works).
- `unset NAME` on a variable inherited from the process environment
  removes it for real — children spawned afterwards no longer see it.
- Interactive multi-line input cannot wedge on a stray `)` any more:
  inside an open group it ends the gather with a single GNU-style
  diagnostic and rc 2, returning to PS1.
- `${...}` expansions containing quotes or command substitutions parse to
  the real closing brace (no early-close split), redirect diagnostics
  name the fd that actually failed through the ordered fd table, and
  negated groups like `! { ...; }` parse.
- Literal private-use-area characters in a `-c` operand pass through
  verbatim — the engine's internal marker zone is only re-entered under
  double-escape, so real U+E3xx bytes in a script are safe.
- The interactive line editor redraws multibyte/CJK prompts correctly
  (vendored Bash patches 17–20 rode in with the engine's upgrade to
  patch 20).

## Decision guidance — niu vs cmd vs powershell

| Task | Use |
| --- | --- |
| Anything with bash syntax (loops, pipes, substitution, globs, scripts) | **niu** — directly in-session |
| Running Windows-native programs (`git.exe`, `node.exe`, `cargo.exe`) | **niu** — your PATH is your PATH, no heuristic conversion step |
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
- When a host cannot safely quote a command line at all, use the verbatim
  handoff — a base64 payload instead of an inline command:
  `niu --encoded-command <b64>` (canonical), `-EncodedCommand <b64>`
  (PowerShell spelling, case-insensitive), or `--ec <b64>`; an optional
  `v1:` prefix on the payload guards it against MSYS-family rewriting in
  transit. `niu -C <cmd>` runs one command with REPL startup semantics,
  and `--cwd-state <file>` lets a relaunching host restore and re-record
  the session directory.

## References

- `references/quickref.md` — generated command tables: launcher verbs,
  plugin verbs, full built-in table, winuxcmd applet inventory.
- `references/paths.md` — the path contract and the MSYS contrast.
- `references/prompt-plugins.md` — rc contract, prompt ownership, the
  external plugin sources and the generated loader.
- `references/wpm.md` — package discovery, links repair, index flows.
