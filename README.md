<p align="center">
  <img src="assets/niubash-banner.svg" alt="niubash — Bash, native on Windows." />
</p>

> **Bash, native on Windows.** No WSL. No VM. No `/mnt/c`. No cmdlet dialect.
> One `niu.exe`: the shell your fingers already know — and the one your AI
> agent actually speaks.

<div align="center">

[English](README.md) · [中文](README-zh.md)

[![niubash CI](https://github.com/unixwin/niubash/actions/workflows/ci.yml/badge.svg)](https://github.com/unixwin/niubash/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/unixwin/niubash)](https://github.com/unixwin/niubash/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64%20%7C%20ARM64-blue)](https://github.com/unixwin/niubash)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://github.com/unixwin/niubash)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/unixwin/niubash)](https://github.com/unixwin/niubash/stargazers)

</div>

**niubash** is a native Windows shell that runs real Bash — no Linux VM, no
emulation layer, no path roulette. One `niu.exe` bundles the
[rubash](https://github.com/unixwin/rubash) language engine, real Unix
commands from [winuxcmd](https://github.com/unixwin/winuxcmd), a git-aware
prompt, and a permission-modeled plugin system.

**Highlights**

- **Real Bash** — `if`, `for`, `case`, `$(...)`, pipes, heredocs, functions, arrays. The [rubash](https://github.com/unixwin/rubash) engine passes **86/86** of GNU Bash's own test suite.
- **Native Windows paths** — any dialect in, Windows-native out. No `/mnt/c`, no MSYS-style path roulette.
- **Unix commands included** — `ls`, `cat`, `grep`, `find`, `sed`, `printf`, … are real winuxcmd binaries on your PATH. Nothing to install.
- **Real Windows programs, direct** — `git.exe`, `node.exe`, `python.exe`, `cargo.exe`. Your PATH is your PATH.
- **Built for AI agents** — models train on Bash; niubash gives them a deterministic Bash contract on Windows.
- **A prompt you'll enjoy** — 27 themes, a git status prompt with teeth, syntax highlighting, autosuggestions, vi/emacs modes.

See it in action:

<div align="center">

<p><a href="https://dl.caomengxuan666.com"><strong>▶ Watch the 43-second film</strong></a> — the full pitch, with sound.</p>

<img src="assets/demo.gif" alt="niubash interactive session: starship prompt, tab completion, grep pipes, heredocs, wpm packages, eza icons" width="720"/>

</div>

## Table of Contents

- [Installation](#installation)
- [Configuration](#configuration)
- [Features](#features)
- [Why not WSL](#why-not-wsl)
- [AI-agent friendly](#ai-agent-friendly)
- [How it compares](#how-it-compares)
- [Architecture](#architecture)
- [FAQ](#faq)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License](#license)

## Installation

Grab the installer `niubash-v*-win-*-setup.exe` from the
[Releases](https://github.com/unixwin/niubash/releases) page and run it — no
admin rights. It wires up your PATH and a Windows Terminal profile. Prefer
portable? Take the `.zip` — the first launch self-activates the Unix
commands.

From source:

```sh
git clone https://github.com/unixwin/niubash.git && cd niubash
cargo build --release && target\release\niu.exe
```

Requirements: **Windows 10/11 x64 or ARM64**, Rust 1.70+ to build from source.

## Configuration

One config file, plain Bash syntax: `~/.niubashrc`. Theme, prompt,
plugins, exports, aliases, and functions all live there:

```bash
NIU_THEME=p10-classic
NIU_THEME_PLUGIN=theme-p10-classic
NIU_PLUGINS=(prompt-core git common-aliases)
export NIU_THEME NIU_THEME_PLUGIN

# the official plugin distribution, oh-my-niu
[ -f "$NIUBASH/oh-my-niu.winux" ] && . "$NIUBASH/oh-my-niu.winux"

alias ll='ls -la'
alias gst='git status'
hello() { echo "hello from niu"; }
```

- **Shared history across shells** — `NIU_HISTORY_MODE` offers `shared` (default), `session`, and `private`.
- **One-shot init file** — `NIU_ENV=<file>` (or bash-compatible `BASH_ENV`) sources a single init file before `niu -c`, scripts, and piped stdin. Unset by default, keeping one-shot runs fast.
- **Keep it current** — `niu --self-update` (or `self-update` inside the shell).

## Features

- **Real Bash semantics** — the [rubash](https://github.com/unixwin/rubash) engine passes **86/86** on GNU Bash's upstream test suite.
- **Native path contract** — any dialect in, Windows-native out. MSYS-style path conversion roulette does not exist here.
- **Unix commands as real binaries** — winuxcmd injects PATH command links; `ls`/`grep` are real Windows processes, not emulation inside the shell.
- **A prompt you'll enjoy** — 27 themes (agnoster, spaceship, tokyonight, the p10 family, ...), a git status prompt that grows teeth (staged / modified / untracked / ahead / behind / stashes / conflicts), syntax highlighting, autosuggestions, vi/emacs modes, Ctrl+R history search.
- **Plugins with a permission model** — 40+ official packs (`git`, `docker`, `kubectl`, `npm`, `zoxide`, `direnv`, `fzf`, `thefuck`, ...), host access declared in manifests, reviewed source packs load only bundle-local declared scripts.
- **Completions** — shell definitions + automatic bash completion import + `cmd -h` description sniffing + three-level caching.
- **Three execution modes** — interactive REPL; one-shot command execution (quiet and deterministic, loads no rc and no plugins); a one-shot REPL command that loads full startup state then exits.
- **Self-update** — the shell (`niu --self-update`), the command layer (`wpm update winuxcmd`), and plugin bundles each update on their own plane.

## Why not WSL

Booting a Linux VM to run `grep` is buying a whole ranch because you wanted
a glass of milk. Nice cows, terrible logistics.

Every Windows shell asks you to give something up. CMD is frozen in 1987.
PowerShell isn't Bash — your `for` loops and quoting instincts die on
arrival. WSL is a whole Linux distro you adopt just to print a directory.
Git Bash emulates Unix and *guesses* at your paths, and Windows-native tools
refuse to speak its dialect.

niubash keeps Bash without the overhead: no distro to patch, no emulation
layer to appease. The full feature set and the side-by-side comparison are
below — [Features](#features) and [How it compares](#how-it-compares).

## AI-agent friendly

Every AI coding agent speaks Bash — models are trained on Bash. On Windows,
most are stuck with PowerShell, the shell that famously *eats arguments*:

```text
# PowerShell 5.1                                # niubash
> node -e "console.log(JSON.stringify(          ❯ node -e "console.log(JSON.stringify(
    process.argv.slice(1)))" "a b" "" "c\"d"     process.argv.slice(1)))" "a b" "" "c\"d"
    "e\f" "---"                                   "e\f" "---"

ParserError: TerminatorExpectedAtEndOfString   ["a b","","c\"d","e\\f","---"]
```

Five arguments in. PowerShell throws a parse error; niubash delivers all five
byte-for-byte. Even [Codex is locked to PowerShell on Windows](https://github.com/openai/codex/issues/31548)
— users are literally voting to escape. The full receipts are in
[Why niubash](docs/src/why-niubash.md).

The one-shot form is a contract, not an afterthought:

- **No banners**, stable stdout/stderr, **exact exit-code propagation** — what the agent writes is what the process receives.
- It loads **no rc, no plugins, no interactive hooks** — today's run and tomorrow's run are the same run.
- **Zero path conversion** — Bash instincts work directly, with none of MSYS's argument-rewriting roulette.
- A model trained on Bash finally doesn't have to learn the local dialect.

To give an agent shell aliases, env vars, or PATH tweaks without the full interactive rc, set `NIU_ENV` (or `BASH_ENV`) to a dedicated init file. Only that file is sourced — no plugins, prompts, or completion machinery:

```bash
# ~/.opencode.env — sourced by `niu -c` when NIU_ENV points here
export PATH="$HOME/tools:$PATH"
alias ll='ls -la'
export DOCKER_CONTEXT=my-cluster
```

```bash
NIU_ENV=~/.opencode.env niu -c 'll | head'
```

Unset `NIU_ENV` / `BASH_ENV` and `-c` stays zero-load and fast.

This is what that feels like from the other side of the keyboard:

<div align="center">

<img src="assets/demo-drama.gif" alt="Animated story: a developer chats with codex, PowerShell eats the arguments, the user loses it, then niubash saves the day" width="560"/>

</div>

## How it compares

| | niubash | WSL | Git Bash | PowerShell | CMD |
|---|---|---|---|---|---|
| Bash syntax | ✅ | ✅ | ✅ | ❌ | ❌ |
| Native Windows paths (no `/mnt/c`) | ✅ | ❌ | ⚠️ conversion quirks | ✅ | ✅ |
| Calls `git.exe` / `node.exe` directly | ✅ | ⚠️ via `/mnt/c` | ⚠️ path translation | ✅ | ✅ |
| Unix commands (`ls`, `grep`, `find`) | ✅ | ✅ | ✅ | ❌ | ❌ |
| Agent-written Bash just runs | ✅ | ✅ | ⚠️ arg rewriting | ❌ | ❌ |
| Cold start to prompt | **~170 ms** | seconds | ~1 s | ~280 ms | — |
| No extra OS, no VM | ✅ | ❌ | ✅ | ✅ | ✅ |
| Themes / git prompt / plugins | ✅ | — | ✅ | ⚠️ | ❌ |

One binary. One process. No distro to patch, no emulation layer to appease.

## Architecture

```
niu.exe
├── niubash host layer (Rust)     reedline line editing · themes · completions · plugins · Ctrl+C
├── rubash engine (lib, Rust)     lexer / parser / executor / builtins
└── winuxcmd.exe command layer (C++)  Unix coreutils as real binaries, PATH command links
```

- **rubash is the engine, and the single authority** — niubash does not implement the shell language itself; rubash is linked directly as a Rust crate. Parsing, execution, builtins, expansion, redirects, pipelines, and job control all live upstream. Semantic bugs get fixed in [rubash](https://github.com/unixwin/rubash), and every Bash user on Windows wins together.
- **winuxcmd is a command layer, not a DLL** — no FFI, no routing magic. It is an ordinary Windows process; rubash finds `ls`, `grep` and friends through the normal Windows PATH.
- **oh-my-niu is the official plugin distribution** — shipped with niubash, manifest-declared permissions, in two shapes: reviewed source packs and process adapters.
- Non-goal: a native Linux/macOS shell product. rubash is portable, but niubash targets Windows — one thing, done extremely well.

## FAQ

- **Another Git Bash?** No — Git Bash emulates Unix on top of Windows: translating paths, guessing at arguments. niubash is a native Windows process; Bash compatibility happens in the language engine (rubash), not in a fake filesystem.
- **Still need WSL?** Sure — for real Linux kernels, Linux Docker, Linux-only toolchains, it's still the right tool. For the other 95% of your day: you don't need WSL. You need niubash.
- **Why the name `niu`?** Short, fast to type, zero finger travel. The project is niubash, the binary is `niu`, the env prefix is `NIU_` — and "niu" (牛) is what your shell should be on Windows.
- **Is this a hit piece on PowerShell?** No. PowerShell is a genuinely powerful automation language — it just isn't Bash. Models are trained on Bash and then forced to speak cmdlet on Windows. The problem is the mismatch, not the people.

## Documentation

Full docs site: **[docs](https://unixwin.github.io/niubash/)** · [Getting started](docs/src/getting-started.md) · [Why niubash](docs/src/why-niubash.md) · [Advanced usage](docs/src/advanced-usage.md) · [Architecture](docs/src/architecture.md)

## Contributing

Bug reports, feature requests, and pull requests are welcome — open an
[issue](https://github.com/unixwin/niubash/issues) or a PR. The docs live
in [`docs/`](docs/) and the sources in [`src/`](src/). Before
submitting, make sure the verification loop passes:
`cargo fmt --check`, `cargo build --locked`, `cargo test --workspace --locked`.

---

If niubash just saved you from booting a Linux VM to run `grep`,
[star the repo](https://github.com/unixwin/niubash) and tell a Windows
developer. ★

## License

MIT. See [LICENSE](LICENSE).
