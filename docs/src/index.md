# Niubash

> **Bash, native on Windows.** No WSL. No VM. No `/mnt/c`. No cmdlet dialect.
> Just the shell your fingers already know — and the one your AI agent
> actually speaks.

This is the Niubash documentation. Start with the guides, or jump straight
into the shell:

```text
C:\Users\you\repo
❯ test -f Cargo.toml && echo "rust, obviously"
rust, obviously

❯ printf "%s\n" alpha beta gamma | grep beta
beta

❯ cd "C:\Program Files"
C:\Program Files
```

## What Niubash is

- **Bash syntax.** `if`, `for`, `case`, `$(...)`, pipes, redirects, heredocs,
  functions, aliases — the whole grammar, powered by
  [rubash](https://github.com/unixwin/rubash), which is gated on the GNU Bash
  project's own 83-suite upstream test corpus (**83 of 83 suites pass, per the
  2026-10-12 full re-run**: 62 byte-identical on Windows, 20 differing only on
  environment-bound lines, and the one nameref residual proven to be GNU
  Bash's own coproc reap-timing race — see the [rubash compatibility
  ledger](https://github.com/unixwin/rubash#compatibility-at-a-glance)).
- **Windows-native.** One binary, one process. Native Windows paths
  (`C:\...` and `C:/...`), direct execution of `git.exe`, `node.exe`,
  `cargo.exe`, `python.exe` — no VM, no emulation layer. The Windows-native
  path is the shell's first-class representation, POSIX- and WSL-style
  spellings are understood on input, and the Rubash executor translates
  POSIX-form arguments deterministically before a native child starts:
  `/d/repo` arrives at `python.exe` as `D:\repo`, while shapes the
  executor does not classify as paths (`/nonexistent` and `/fn main`)
  arrive byte for byte.
  The native command processor keeps its `/C`-style switch-position
  operands verbatim. The full contract — the exact rules, the `cmd.exe`
  carve-out, and the `__RUBASH_ARGV_DIALECT=legacy` pass-through switch —
  is stated in the
  [Windows Path Contract](windows-path-contract.md).
- **Unix commands included.** WinuxCmd ships `ls`, `cat`, `grep`, `find`,
  `test`, `printf`, and friends as Windows command links, with no separate
  installation.
- **AI-native.** `niu -c` is a contract: no banners, stable
  stdout/stderr, exact exit-code propagation. What an agent writes is what
  the process receives — the quoting roulette of PowerShell and the path
  mangling of MSYS/Git Bash are both gone.
- **A prompt you'll enjoy.** The real theme ecosystems — oh-my-bash's and
  bash-it's catalogs (agnoster, spaceship, pure, the p10 family, ...) — plus a
  git prompt that grows teeth inside any repository, syntax highlighting,
  autosuggestions, vi and emacs modes, and Ctrl+R history search.
- **The bash ecosystem as the plugin system.** oh-my-bash, bash-it,
  bash-completion and any git source install through `niu plugin` —
  nothing is sourced until you enable it. A ~540-row
  recipe index (`niu plugin recipe list`) covers themes, plugins, aliases,
  completions and prompt tools.

## The documentation

| Guide | What it covers |
|-------|----------------|
| [Getting Started](getting-started.md) | Zero to a git prompt in ten minutes |
| [Advanced Usage](advanced-usage.md) | Execution modes, startup files, themes, plugins, completion, debugging |
| [Install & Self-Update](installer.md) | Installer, portable zip, Windows Terminal profile, updates |
| [Bash Compatibility Matrix](rubash-bash-compat-matrix.md) | What Bash surface is verified, layer by layer |
| [Performance](performance.md) | Every number pinned to the issue or PR that measured it |
| [Built-ins & Fast Paths](builtins.md) | All 64 built-ins, fast-path fallbacks, WinuxCmd overlap |
| [Architecture](architecture.md) | rubash + WinuxCmd + reedline, path model, host contract |
| [Windows Path Contract](windows-path-contract.md) | logical root, dispatcher selection, and layer ownership |
| [Roadmap](niubash-roadmap.md) | What is done, what is next |

## Get it

Download the installer from
[GitHub Releases](https://github.com/unixwin/niubash/releases), install it
with winget (`winget install --id Unixwin.Niubash`), or build
from source:

```sh
git clone https://github.com/unixwin/niubash.git
cd niubash
cargo build --release
target\release\niu.exe
```

Keep it current with `niu --self-update` (or `self-update` inside the
shell) and `wpm update winuxcmd` for the Unix command set. Questions and bug
reports go to
[github.com/unixwin/niubash/issues](https://github.com/unixwin/niubash/issues).

Shell semantics live upstream in
[rubash](https://github.com/unixwin/rubash) — fix the engine, and every
Bash user on Windows wins.
