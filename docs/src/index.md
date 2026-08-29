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
  [rubash](https://github.com/unixwin/rubash), which passes the GNU Bash
  project's own test suite (**86/86 upstream tests green**).
- **Windows-native.** One binary, one process. Native Windows paths
  (`C:\...` and `C:/...`), direct execution of `git.exe`, `node.exe`,
  `cargo.exe`, `python.exe` — no VM, no emulation layer, no path conversion
  roulette.
- **Unix commands included.** WinuxCmd ships `ls`, `cat`, `grep`, `find`,
  `test`, `printf`, and friends as Windows command links, with no separate
  installation.
- **AI-native.** `niu -c` is a contract: no banners, stable
  stdout/stderr, exact exit-code propagation. What an agent writes is what
  the process receives — the quoting roulette of PowerShell and the path
  mangling of MSYS/Git Bash are both gone.
- **A prompt you'll enjoy.** 27 bundled themes (agnoster, spaceship, pure,
  catppuccin-mocha, tokyonight, p10 family, ...), a git prompt that grows
  teeth inside any repository, syntax highlighting, autosuggestions, vi and
  emacs modes, and Ctrl+R history search.
- **Plugins with a permission model.** 40+ bundled packs (`git`, `docker`,
  `kubectl`, `npm`, `zoxide`, `direnv`, `fzf`, `thefuck`, ...), with reviewed
  source packs and process adapters declaring the host access they need.

## The documentation

| Guide | What it covers |
|-------|----------------|
| [Getting Started](getting-started.md) | Zero to a git prompt in ten minutes |
| [Advanced Usage](advanced-usage.md) | Execution modes, startup files, themes, plugins, completion, debugging |
| [Install & Self-Update](installer.md) | Installer, portable zip, Windows Terminal profile, updates |
| [Bash Compatibility Matrix](rubash-bash-compat-matrix.md) | What Bash surface is verified, layer by layer |
| [Architecture](architecture.md) | rubash + WinuxCmd + reedline, path model, host contract |
| [Windows Path Contract](windows-path-contract.md) | logical root, dispatcher selection, and layer ownership |
| [Roadmap](niubash-roadmap.md) | What is done, what is next |

## Get it

Download the installer from
[GitHub Releases](https://github.com/unixwin/niubash/releases), or build
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
