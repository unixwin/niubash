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
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-blue)](https://github.com/unixwin/niubash)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://github.com/unixwin/niubash)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/unixwin/niubash)](https://github.com/unixwin/niubash/stargazers)

</div>

**niubash** is a native Windows shell that runs real Bash — no Linux VM, no
emulation layer, no path roulette. One `niu.exe` bundles the
[rubash](https://github.com/unixwin/rubash) language engine, real Unix
commands from [winuxcmd](https://github.com/unixwin/winuxcmd), a git-aware
prompt, and a permission-modeled plugin system.

**What it is — and is not.** niubash is a bash-compatible shell implemented
natively in Rust for Windows: the language engine
([rubash](https://github.com/unixwin/rubash)) is a from-scratch Bash
interpreter, and the bundled Unix commands are native Windows executables.
It is **not MSYS2, not Cygwin, not Git Bash, and not WSL** — there is no
POSIX emulation layer, no `cygwin1.dll` / `msys-2.0.dll`, and no
path-translation machinery anywhere in the stack. Every process niubash
starts is an ordinary Win32 process, and niubash itself has **no runtime
dependency on Python, Node.js, or any other language toolchain** — nor any
embedded downloader: plugin sources arrive by git clone only, and optional
fonts and CLI tools are recommendations for your package manager, never
background fetches.

**There is no path-conversion layer — by design.** MSYS-family shells live
in a Unix-looking world and must heuristically translate to Windows, and
since a heuristic that always guesses right does not exist, they ship
escape hatches (`MSYS_NO_PATHCONV`, `MSYS2_ARG_CONV_EXCL`) for when the
guessing breaks your command — an off switch is a confession that the
layer misfires. Niubash has nothing to switch off: the **Windows-native
path is the shell's first-class internal representation**. `/c/...`,
`/mnt/c/...`, and `C:\...` are all understood as input spellings of that
one reality; what any process receives is always a native Windows path.
Native Windows programs cannot hit a path problem coming from this shell —
there is no translation step left to get wrong.

**Highlights**

- **Real Bash** — `if`, `for`, `case`, `$(...)`, pipes, heredocs, functions, arrays. The [rubash](https://github.com/unixwin/rubash) engine is gated on GNU Bash's own upstream test suite — the measured record is in [How it compares](#how-it-compares).
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

On Linux (x86_64, aarch64; glibc 2.35+) and macOS (aarch64, x86_64), grab
the portable tarball from the
[Releases](https://github.com/unixwin/niubash/releases) page —
`niubash-v*-linux-x86_64.tar.gz`, `niubash-v*-linux-aarch64.tar.gz`,
`niubash-v*-macos-aarch64.tar.gz`, or `niubash-v*-macos-x86_64.tar.gz` —
then untar and run `./niu` (native system tools are used, nothing is
bundled):

```sh
tar -xzf niubash-v*-linux-x86_64.tar.gz && niubash-v*-linux-x86_64/niu
```

Each release artifact is smoke-verified on its build OS before upload
([release pipeline](docs/release.md)).

From source:

```sh
git clone https://github.com/unixwin/niubash.git && cd niubash
cargo build --release && target\release\niu.exe
```

Requirements: **Windows 10/11 x64 or ARM64**, **Linux x86_64/aarch64
(glibc 2.35+)**, or **macOS aarch64/x86_64**; Rust 1.70+ to build from
source.

## Configuration

One config file, plain Bash syntax: `~/.niubashrc`. Exports, aliases, and
functions live there; themes and plugins come from the external ecosystem
(`niu plugin`):

```bash
# Optional floor knobs — they only shape the built-in default prompt and
# completion menu; an enabled external theme claims PS1 and wins.
# NIU_PROMPT_CWD_STYLE='home'    # home | full | basename
# NIU_COMPLETION_STYLE='column'  # ide | column | list | inline

alias ll='ls -la'
alias gst='git status'
hello() { echo "hello from niu"; }

# Themes/plugins, managed by marker-delimited blocks at the end of the rc:
#   niu plugin add oh-my-bash && niu plugin trust oh-my-bash
#   niu plugin enable oh-my-bash && niu plugin enable agnoster
```

- **Shared history across shells** — `NIU_HISTORY_MODE` offers `shared` (default), `session`, and `private`.
- **One-shot init file** — `NIU_ENV=<file>` (or bash-compatible `BASH_ENV`) sources a single init file before `niu -c`, scripts, and piped stdin. Unset by default, keeping one-shot runs fast.
- **Keep it current** — `niu --self-update` (or `self-update` inside the shell).

### Setting the theme correctly

Pick the theme through the declarative spec, not by hand-editing the rc:
in `~/.niubash/plugins.toml`, set `theme = "..."` on the source entry —

```toml
[[sources]]
target = "oh-my-bash"
enable = ["git"]
theme  = "agnoster"
```

Do **not** hand-write `export OSH_THEME=agnoster` in `~/.niubashrc`: that
line lives outside (or inside) the marker-delimited managed blocks that
`niu plugin sync` rewrites from the spec, so the next sync drops or
overrides it and the theme silently falls back to the default. If sync
detects such a hand-written line it prints a one-time warning on stderr
pointing at the spec. Inside the shell, `niu plugin enable <theme>` or the
`niu setup` wizard writes the spec for you.

## Features

- **Real Bash semantics** — the [rubash](https://github.com/unixwin/rubash) engine is gated on GNU Bash's own upstream test suite; the dated, measured record lives in [How it compares](#how-it-compares).
- **Native path contract** — any dialect in, Windows-native out. MSYS-style path conversion roulette does not exist here.
- **Unix commands as real binaries** — winuxcmd injects PATH command links; `ls`/`grep` are real Windows processes, not emulation inside the shell.
- **A prompt floor plus any theme you like** — a fast built-in default prompt that yields the moment something claims `PS1`: enable oh-my-bash and its themes (agnoster, robbyrussell, ...) render through the bash-compatible channel, or drop in starship. Syntax highlighting, autosuggestions, vi/emacs modes, Ctrl+R history search.
- **The bash plugin ecosystem, managed** — oh-my-bash, bash-it, and bash-completion run through their own native loaders (no shims), driven by a declarative spec (`~/.niubash/plugins.toml`) with `niu plugin sync` reconciliation and a commit-pinned lockfile. Any git URL or local path works too, and every source sits behind an explicit trust gate — `niu plugin enable` writes one guarded, marker-delimited loader block per source into your rc.
- **Zero-download by design** — the shell itself fetches nothing except git clones: the embedded downloader is gone, and optional fonts and CLI tools are recommendations for your package manager, never background fetches.
- **Completions** — shell definitions + automatic bash completion import + `cmd -h` description sniffing + three-level caching.
- **Three execution modes** — interactive REPL; one-shot command execution (quiet and deterministic, loads no rc and no plugins); a one-shot REPL command that loads full startup state then exits.
- **Self-update** — the shell (`niu --self-update`), the command layer (`wpm update winuxcmd`), and plugin sources (`niu plugin update`) each update on their own plane.

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
— users are voting to escape. The full receipts are in
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
| Implementation | native Rust engine, native PE commands | full Linux distro in a VM | POSIX emulation (`msys-2.0.dll`) | native | native |
| Bash syntax | ✅ | ✅ | ✅ | ❌ | ❌ |
| Native Windows paths (no `/mnt/c`) | ✅ | ❌ | ⚠️ heuristic conversion, corrupts args | ✅ | ✅ |
| Calls `git.exe` / `node.exe` directly | ✅ | ⚠️ via `/mnt/c` | ⚠️ path translation mangles args | ✅ | ✅ |
| Unix commands (`ls`, `grep`, `find`) | ✅ | ✅ | ✅ | ❌ | ❌ |
| Agent-written Bash just runs | ✅ | ✅ | ⚠️ arg rewriting | ❌ | ❌ |
| Cold start to prompt | **~170 ms** | seconds | ~1 s | ~280 ms | — |
| No extra OS, no VM | ✅ | ❌ | ✅ | ✅ | ✅ |
| Themes / git prompt / plugins | ✅ | — | ✅ | ⚠️ | ❌ |

One binary. One process. No distro to patch, no emulation layer to appease.
Emulation was the previous century's approximation of the right
architecture — a native implementation where the Windows path is the only
path that exists. That architecture ships here.

### The closest comparable: [brush](https://github.com/reubeno/brush)

Credit where due: brush pioneered "bash, re-implemented in Rust" and took it
further than anyone else — it is the honest reference point for this
category. The head-to-head:

| | niubash | brush |
|---|---|---|
| Approach | bash re-implemented in Rust, Windows-native | bash re-implemented in Rust, cross-platform |
| Compatibility gate | GNU Bash's **own upstream test suite** — 86/86 gate green, **83/83** full suites byte-identical (zero-diff; measured 2026-09-21, see below) | GNU's suite is not run at all; validation is a self-built 1700+ case corpus with bash as oracle, ~125 known failures ([their reference](https://github.com/reubeno/brush/blob/main/docs/reference/compatibility.md)) |

**Same exam, same proctor — measured, not claimed.** We ran GNU Bash's 83
upstream test suites through the identical bridge harness
([`run-83.sh`](https://github.com/unixwin/rubash/blob/master/tests/gnu-compat/run-83.sh),
same baselines from WSL GNU Bash, same output normalization, brush built in
release mode): **niubash 83/83 byte-identical, brush 10/83** — with 3 suites
brush could not finish inside the 150-second bound
(measured 2026-09-21, brush v0.4.0).

Why such a gap? The two test counts measure different instruments. A
self-built corpus of ~1,700 curated single-case checks answers "does this
construct roughly work?"; GNU's 83 whole-behavior suites replay bash's own
torture tests — traps, history expansion, POSIX mode, exotic redirection —
and demand **byte-identical** output. The bridge above is what "~1700
tests" buys under the official instrument. And byte-identical is the bar
that matters for scripts and agents: "close enough" breaks the moment
output is piped into the next command.
| Unix commands on Windows | bundled: `ls`, `cat`, `grep`, `find`, `sed`, … via [winuxcmd](https://github.com/unixwin/winuxcmd) | none bundled — you still need external tools for `ls` |
| Path model | Windows-native paths first-class; `/c/…` and `/mnt/c/…` are input dialects; no conversion layer exists | generic cross-platform path handling |
| Interactive surface | IDE-style completion menu, 27 themes, plugin ecosystem (oh-my-niu) | syntax highlighting, autosuggestions, starship |

Same idea, different depth. Brush proves the approach works; niubash ships
it complete on Windows — engine, commands, path contract, and ecosystem
together.

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

- **Is niubash based on MSYS2 or Cygwin?** No. MSYS2 and Cygwin are POSIX
  emulation layers: a Unix-ish DLL runtime, a fake root filesystem, and
  heuristic path translation that rewrites your arguments at the worst
  moment. niubash has none of that — Bash compatibility lives in the
  language engine ([rubash](https://github.com/unixwin/rubash)), commands
  are native Windows executables, and `C:\`/`C:/` paths pass through
  untranslated. That's why MSYS-world needs `MSYS_NO_PATHCONV` to turn its
  converter off — and why niubash has no equivalent flag: there is no
  converter to disable. (The `bash.exe`/`sh.exe` in the install folder are
  tiny forwarder shims that start `niu.exe` — not MSYS bash.)
- **Another Git Bash?** No — Git Bash emulates Unix on top of Windows: translating paths, guessing at arguments. niubash is a native Windows process; Bash compatibility happens in the language engine (rubash), not in a fake filesystem.
- **Still need WSL?** Sure — for real Linux kernels, Linux Docker, Linux-only toolchains, it's still the right tool. For the other 95% of your day: you don't need WSL. You need niubash.
- **Why the name `niu`?** Short, fast to type, zero finger travel. The project is niubash, the binary is `niu`, the env prefix is `NIU_` — and "niu" (牛) is what your shell should be on Windows.
- **Is this a hit piece on PowerShell?** No. PowerShell is a powerful automation language — it just isn't Bash. Models are trained on Bash and then forced to speak cmdlet on Windows. The problem is the mismatch, not the people.

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
