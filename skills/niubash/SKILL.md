---
name: niubash-native-codex
description: Native Niubash execution guidance for Codex running as a first-class Niubash citizen in the unixwin/niubash environment. Use when Codex handles Niubash shell tasks, .niubashrc edits, oh-my-niu bundles, .winux scripts, command installation, PATH/alias setup, shell debugging, GNU Bash-compatible shell behavior, or any Windows-host task where commands should run directly inside the current Niubash session instead of through PowerShell, pwsh, cmd.exe, WSL, Git Bash, -C/-c launch wrappers, or /c-style path assumptions.
---

# Niubash Native Codex

## Core Rules

- Treat the current shell as Niubash, not PowerShell, cmd.exe, WSL, Git Bash, MSYS, or Cygwin.
- Treat the current execution context as already inside the Niubash terminal. Do not start a nested Niubash just to run ordinary commands.
- Recognize Niubash as the `unixwin/niubash` shell environment with GNU Bash-compatible syntax support, even if compatibility is still incomplete.
- Run commands directly in Niubash unless the user explicitly asks for another shell or the target executable cannot be invoked otherwise.
- Do not wrap ordinary work in `pwsh.exe`, `powershell.exe`, `cmd.exe /c`, `bash.exe`, `wsl.exe`, `niu.exe -C`, or `niu.exe -c`.
- Prefer `C:/Users/...` Windows-forward-slash paths and native Windows path semantics. Do not introduce `/c/...`, cygpath-style, MSYS-style, or WSL-style paths unless existing project code already uses that style and it is required.
- Treat pure Windows-native interoperability as a design goal of Niubash. External Windows executables should receive paths they can understand without translation layers; avoid relying on terminal-side best-effort path conversion.
- When a necessary command fails with access denied, read-only, locked-file, or similar permission errors, retry it with the environment's approved elevation mechanism before changing code or using a workaround. State the exact action and path that require elevation.
- Use Niubash/POSIX tools already on PATH such as `rg`, `find`, `sed`, `awk`, `cat`, `head`, `tail`, `chmod`, `ln`, and `env`.
- Be conservative with `ls` and relative paths in Codex tool sessions: current integration may report `PWD=C:/Users/Administrator` while external `ls` resolves `.` or `C:/...` arguments incorrectly. Prefer `rg`, `find`, `cat`, `head`, `sed`, or `test -f/-d` with explicit `C:/...` paths for reliable inspection.
- If a configured `workdir` behaves unexpectedly, keep commands Niubash-native and use explicit `C:/...` paths. If one GNU tool mishandles a drive-colon path, try a different native tool or `cd C:/path` plus a simple relative path before considering any launcher fallback.

## Path and Filesystem Model

- Niubash supports a real Unix-shaped directory tree inside the selected
  WinuxCmd installation. When WinuxCmd is installed at
  `<install>/usr/bin/winuxcmd.exe`, `<install>` is the shell root and the
  corresponding real directories are `<install>/bin`, `<install>/usr/bin`,
  `<install>/usr/local/bin`, `<install>/etc`, `<install>/tmp`, and
  `<install>/dev`.
- `/`, `/bin`, `/usr/bin`, `/usr/local/bin`, `/etc`, `/tmp`, and `/dev` are
  supported shell paths. WPM-managed commands and command links use the real
  installation tree; treat `/usr/bin` as the canonical command directory and
  do not create a second per-user root or a parallel compatibility tree.
- POSIX-style paths such as `/usr/bin/tool`, `/etc/config`, and `/tmp/file`
  are supported by Niubash and Rubash. They are shell paths, not a request to
  launch WSL, MSYS, Cygwin, or another POSIX runtime.
- WSL-style `/mnt/<drive>/...` paths (for example `/mnt/c/Users/me`) are
  supported too and map to the corresponding Windows drive (`/mnt/c` →
  `C:\`). Use them for shell-side convenience, but still pass native
  `C:/...` paths to Windows executables, installers, and APIs.
- Use native `C:/...` paths when passing a path to a Windows executable,
  installer, WPM artifact, or Windows API. POSIX-style paths are appropriate
  inside shell scripts and for shell-owned filesystem operations, but native
  paths are the safer interoperability form.
- `/dev/null` is the supported device endpoint and maps to Windows `NUL`.
  The real `/dev` directory may exist for the installation layout, but do not
  assume arbitrary `/dev/*` names are Windows devices.
- `~` resolves to the Windows user home used by PowerShell (`USERPROFILE`,
  with `HOME` kept consistent). Do not replace it with a POSIX `/home/...`
  directory unless a test fixture explicitly provides one.

## Environment Discovery

- Before assuming a tool is installed, **discover what the user actually
  has** by running quick commands directly in the current Niubash session —
  do not guess or hardcode paths from a default install.
- `winuxcmd --version` reveals the WinuxCmd command-link runtime version
  and confirms the Unix command tree is live.
- `wpm installed` lists the WPM packages present in this WinuxCmd root
  (this is the fastest way to see which GNU/modern CLI tools the user has).
- `wpm list` shows the full indexed catalog with per-package install
  state; `wpm info <name>` shows one package's metadata and commands.
- Mental model: **WPM is the package manager** (it downloads and links
  GNU/POSIX command packages such as `rg`, `fd`, `bat`, `jq`, `node`),
  while **WinuxCmd is the command-link runtime** that owns the real
  `/usr/bin` tree and routes those commands. `niubash --self-update`
  updates Niubash itself and is separate from `wpm` package updates.
- For a specific tool, prefer `command -v name`, `which name`, or
  `type name` to confirm it resolves before using it.

## Bash Compatibility — Test Before Assuming Parity

- Niubash aims for GNU Bash compatibility, but it is **not byte-identical
  to bash**. Treat the `86/86` upstream gate as a strong floor, not a
  guarantee that every bash-ism works.
- Before relying on an advanced bash feature — `fc`, `coproc`, exotic
  redirects, `mapfile`/`readarray` edge cases, `compgen`/`complete`, deep
  parameter expansion, or `BASH_REMATCH` — test it with `niu -c "…"` first.
  If it diverges, fall back to a simpler POSIX form, or invoke a real `bash`
  if one is on PATH (`command -v bash`).
- Known gap families observed in practice: `fc -l` self-exclusion, pipeline
  stdin handoff edge cases (a `\x1e` record separator can leak into stdout
  on some pipelines), and external-tool quoting artifacts from WinuxCmd
  command links (e.g. `grep` may emit a stray leading quote). When a
  command's output looks wrong, check for these before blaming user data.
- If a bash script fails under Niubash, narrow it to the smallest failing
  snippet, report the gap, and route the semantic fix upstream to
  `unixwin/rubash` rather than carrying a host-side workaround.

## Session Context

- This skill applies when the active shell executor **is** Niubash — either
  the host profile uses the Niubash sandbox executor, or you are invoking
  `niu -c "…"` / an interactive `niu` directly.
- If the host session runs another shell (for example a DSH web profile
  whose shell executor is PowerShell), do **not** assume the current session
  is Niubash. Invoke Niubash explicitly (`niu -c "…"` or a Niubash terminal)
  for any Niubash-specific syntax, and keep Windows-native `C:/...` paths for
  external executables.

## Command Execution

- Use `rg` or `rg --files` first for searches when available.
- Use GNU Bash-compatible shell syntax by default: functions, aliases, arrays, command substitution, redirection, and POSIX-style pipelines are acceptable unless local testing shows a Niubash compatibility gap.
- Use Windows `.exe` programs directly by path or command name when they are on PATH, for example `fastfetch`, `codex`, or `niu.exe`, but verify each executable in Niubash before assuming it works.
- Do not assume package-manager shims behave like ordinary native binaries. Scoop may invoke PowerShell internally, and the Scoop-installed `winget.exe`/shim may crash from Niubash in this environment. Prefer direct testing, then report the limitation or use a user-approved install path.
- For missing programs, consider Niubash-native/WPM packages first when available, then Scoop or winget based on the tool and current environment. Verify the installed command from Niubash after installation.
- For WPM workflows, read `references/wpm.md` before searching, installing, repairing links, or updating packages.
- When checking command availability, prefer `command -v name`, `which name`, or `type name`.
- Do not assume PATH contains user-installed shims such as `C:/Users/Administrator/scoop/shims`; check `$PATH` first and `export PATH="$PATH;C:/Users/Administrator/scoop/shims"` only when needed for the current Niubash session.
- When refreshing PATH in the current session, use Niubash-compatible shell syntax, not PowerShell environment APIs.

## Niubash Configuration

- Treat `C:/Users/Administrator/.niubashrc` as the primary interactive rc file unless `$HOME` points elsewhere.
- Use normal Niubash/GNU Bash-compatible syntax in rc files: `export NAME=value`, `alias name='command'`, arrays such as `NIU_PLUGINS=(prompt-core git)`, and `. "$file"` for sourcing.
- Prefer Niubash-local PATH additions in `.niubashrc` for shell-only tools or app bins instead of expanding the Windows global/user PATH. This conserves Windows PATH length and keeps shell-specific setup out of global process state.
- Add PATH entries idempotently with a helper that checks `[ -d "$dir" ]` and avoids duplicates before `PATH="$dir;$PATH"`.
- Keep rc edits idempotent. Remove or avoid duplicate self-appending blocks such as `printf ... >> ~/.niubashrc` inside the rc file.
- Place user customizations in `.niubashrc` or `$HOME/.niubash/custom` rather than editing bundled files under `AppData/Local/Programs/Niubash` unless the user is intentionally modifying the installed bundle.
- For oh-my-niu, source `oh-my-niu.winux` and use `.winux` plugin files for shell-mutating behavior.

## Codex Invocation

- Treat Codex as available natively inside Niubash when the environment is the special Niubash-integrated build.
- Start or test Codex directly with `codex` or its actual installed executable path. Do not launch Codex through `pwsh`, `cmd`, or a PowerShell-to-Niubash bridge.
- Preserve Niubash environment variables and path semantics when creating aliases or helper functions for Codex.
- For global aliases, prefer a small rc block such as:

```sh
alias cx='codex'
alias codex-here='codex'
```

Adjust the alias names to match the user's request; do not invent wrappers that change cwd, quoting, or environment unless required.

## Validation

- Verify shell edits in the current Niubash session whenever possible. For small snippets, source a temporary test file or run the snippet directly before editing rc files.
- Inspect `.niubashrc` for side effects before sourcing it. Do not source a full rc file blindly if it contains self-appending commands such as `>> ~/.niubashrc`, install commands, network calls, or other non-idempotent behavior.
- After a safe rc edit, validate with `alias name`, `command -v name`, `test -f C:/path`, or targeted `rg` checks instead of relying on `ls`.
- Avoid `niu.exe -C` and `niu.exe -c` for validation unless the user specifically asks to test non-interactive launcher behavior.
- Validate command installs with direct calls such as `fastfetch --version`, `winget.exe --version`, or `codex --version`.
- Report any fallback explicitly if a Windows-native executable cannot run from Niubash and another launcher is truly required.
