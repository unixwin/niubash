# Discovering WinuxCmd And Utilities

Use this reference when Winuxsh work needs Unix-style tools. Discover the
active command surface instead of assuming a fixed `/usr/bin` inventory.
Winuxsh integrates WinuxCmd by putting the directory containing `winuxcmd.exe`
and its command links on PATH; rubash then resolves external commands normally.

## Locate WinuxCmd

```bash
command -v winuxcmd.exe
cmd_dir=$(dirname "$(command -v winuxcmd.exe)")
printf "%s\n" "$cmd_dir"
```

Winuxsh's search order for WinuxCmd is:

1. `WINUXCMD_PATH`, pointing to `winuxcmd.exe` or its directory.
2. `winuxcmd.exe` beside the active `winuxsh.exe`.
3. `winuxcmd/winuxcmd.exe` beside the active `winuxsh.exe`.
4. `utils/winuxcmd/winuxcmd.exe` beside the active `winuxsh.exe`.
5. `winuxcmd.exe` found through Windows PATH.

Managed `~/.winshrc.toml` can also set `[winuxcmd].path`; read `config.md`
before changing user config.

## Identify The Active Utility

```bash
command -v ls
command -v cat
command -v grep
command -v find
command -v pathchk
ls --version 2>/dev/null || ls --help | head -20
grep --version 2>/dev/null || grep --help | head -20
```

Users may have WinuxCmd, GNU coreutils, other Rust coreutils builds, BusyBox, Git, Node-installed
CLIs, or other providers on PATH. Windows `find.exe` is especially
collision-prone; inspect `command -v find` before assuming GNU-style `find`
flags.

## Probe WPM First

Modern WinuxCmd exposes WPM either as `wpm` or through `winuxcmd.exe wpm`:

```bash
wpm --version || winuxcmd.exe wpm version
winuxcmd.exe wpm links list --root "$cmd_dir"
winuxcmd.exe wpm index status --root "$cmd_dir"
winuxcmd.exe wpm list --root "$cmd_dir"
```

Use WPM when tools are missing or stale:

```bash
winuxcmd.exe wpm search jq --root "$cmd_dir"
winuxcmd.exe wpm info jq --root "$cmd_dir"
winuxcmd.exe wpm install jq --root "$cmd_dir"
winuxcmd.exe wpm update winuxcmd --root "$cmd_dir"
```

`wpm update winuxcmd` updates WinuxCmd packages. `winuxsh --self-update`
updates the shell itself. Keep these mechanisms distinct.

Some builds print the same generic help for `winuxcmd.exe wpm --help` and
subcommand help. Prefer direct, side-effect-free probes such as `version`,
`index status`, `list`, `search`, `info`, and `links list` before assuming a
subcommand is missing.

## Rebuild Command Links

If WPM exists but common command links such as `ls.exe`, `grep.exe`, or
`wpm.exe` are missing, rebuild links in the selected WinuxCmd root:

```bash
winuxcmd.exe wpm links rebuild --root "$cmd_dir" --force
```

Run this only for the WinuxCmd directory the user or release bundle actually
uses. Do not modify an unrelated development or winget install. In read-only or
no-edit validation, do not rebuild; report the missing links and provide the
rebuild command as the next action.

## Activation Failure Diagnostics

If startup prints `winuxcmd activation failed`, keep diagnostics read-only until
the user permits repair:

```bash
command -v winuxcmd.exe || true
cmd_dir=$(dirname "$(command -v winuxcmd.exe)")
printf "%s\n" "$cmd_dir"
ls "$cmd_dir" | head -40
winuxcmd.exe wpm version
winuxcmd.exe wpm links list --root "$cmd_dir"
winuxcmd.exe wpm index status --root "$cmd_dir"
winuxcmd.exe wpm list --root "$cmd_dir" | head -80
for c in ls grep cat find wpm; do printf "%s=" "$c"; command -v "$c" || true; done
```

If `winuxcmd.exe` is present but command links are missing, the likely next
step is `winuxcmd.exe wpm links rebuild --root "$cmd_dir" --force`. If the
selected root is a repo bundle or release artifact, say so explicitly before
changing it.

## Get Help

```bash
winuxcmd.exe wpm --help
winuxcmd.exe grep --help
grep --help
man grep
```

From Codex/pwsh, keep help probes inside Winuxsh:

```powershell
winuxsh -c 'winuxcmd.exe wpm --help | head -80'
winuxsh -c 'grep --help | head -40'
```

## Avoid Host-Shell Alias Collisions

Pwsh aliases can shadow names such as `ls`, `cat`, and `man`. To inspect
Winuxsh behavior, run the lookup inside Winuxsh:

```powershell
winuxsh -c 'command -v ls; ls -la'
winuxsh -c 'command -v cat; cat --help | head -20'
```

If a user intentionally disables WinuxCmd and relies on another provider,
respect that setup for user tasks. For this repository's release-style tests,
missing `ls`, `grep`, `tr`, or similar commands usually means WinuxCmd command
links need repair.
