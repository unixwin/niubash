# Winuxsh

> **Bash, native on Windows.** No WSL. No VM. No `/mnt/c`. No cmdlet dialect.
> Just the shell your fingers already know — and the one your AI agent actually speaks.

[English](README.md) · [中文](README-zh.md)

[![Winuxsh CI](https://github.com/unixwin/winuxsh/actions/workflows/ci.yml/badge.svg)](https://github.com/unixwin/winuxsh/actions/workflows/ci.yml)
[![GPL-3.0](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/unixwin/winuxsh)](https://github.com/unixwin/winuxsh/stargazers)

One native Windows binary. Bash syntax. Windows paths. Real Windows programs.
Unix commands included. No emulation layer between your command and your
tools — and nothing for your AI agent to trip over.

<img src="assets/demo.gif" alt="Winuxsh interactive session: git prompt, grep, sed in-place editing, awk pipelines, tree" width="760"/>

That's it. That's the whole pitch.

## The pitch

**You don't need WSL. You need Winuxsh.**

Every Windows shell asks you to give something up. CMD is frozen in 1987.
PowerShell isn't Bash — your `for` loops and quoting instincts die on
arrival. WSL is a whole Linux distro you adopt just to print a directory.
Git Bash emulates Unix and *guesses* at your paths, and Windows-native tools
refuse to speak its dialect.

Winuxsh gives it all back:

- **Bash, for real** — `if`, `for`, `case`, `$(...)`, pipes, heredocs, functions, arrays. The engine is [rubash](https://github.com/unixwin/rubash), which passes GNU Bash's own test suite: **86/86**.
- **Windows paths, native** — `C:\...` and `C:/...` work as-is, `/c/...` input is understood, and output is always native. Native tools get native paths, zero guessing.
- **Unix commands included** — `ls`, `cat`, `grep`, `find`, `test`, `printf`, ... via WinuxCmd. Nothing to install.
- **Real Windows programs, direct** — `git.exe`, `node.exe`, `python.exe`, `cargo.exe`. Your PATH is your PATH.
- **A prompt you'll enjoy** — 27 themes (agnoster, spaceship, tokyonight, p10 family...), a git prompt that grows teeth, syntax highlighting, autosuggestions, vi/emacs modes.
- **Plugins with a permission model** — 40+ packs (`git`, `docker`, `kubectl`, `npm`, `zoxide`, `fzf`, `thefuck`, ...); reviewed source packs and process adapters declare the host access they need.

## AI-native

Every AI coding agent speaks Bash. On Windows, most are stuck with PowerShell
— the shell that famously *eats arguments*:

```text
# PowerShell 5.1                                # Winuxsh
> node -e "console.log(JSON.stringify(          ❯ node -e "console.log(JSON.stringify(
    process.argv.slice(1)))" "a b" "" "c\"d"      process.argv.slice(1)))" "a b" "" "c\"d"
    "e\f" "---"                                   "e\f" "---"

ParserError: TerminatorExpectedAtEndOfString   ["a b","","c\"d","e\\f","---"]
```

Five arguments in. PowerShell throws a parse error; Winuxsh delivers all five
byte-for-byte. Even [Codex is locked to PowerShell on Windows](https://github.com/openai/codex/issues/31548) — users are literally voting to escape.
[Why Winuxsh](docs/src/why-winuxsh.md) has the full receipts.

This is what that feels like from the other side of the keyboard:

<img src="assets/demo-drama.gif" alt="Animated story: a developer chats with codex, PowerShell eats the arguments, the user loses it, then winuxsh saves the day" width="520"/>

`winuxsh -c` is a contract, not an afterthought: **no banners, stable
stdout/stderr, exact exit-code propagation.** What the agent writes is what
the process receives.

```sh
winuxsh -c 'test -f Cargo.toml && echo build' && echo "exit=$?"
winuxsh deploy.sh
```

## Install

Grab `winuxsh-v*-win-*-setup.exe` from [Releases](https://github.com/unixwin/winuxsh/releases) and run it — no admin rights; it wires up your PATH and a Windows Terminal profile. Or take the portable zip (first launch self-activates the Unix commands). From source:

```sh
git clone https://github.com/unixwin/winuxsh.git && cd winuxsh
cargo build --release && target\release\winuxsh.exe
```

Keep it current: `winuxsh --self-update`.

## Configuration

`~/.winuxshrc` is the interactive entry point. Put your theme, prompt,
plugins, exports, aliases, and functions there:

```sh
WINUXSH_THEME=spaceship
WINUXSH_PLUGINS=(prompt-core git)
[ -f "$WINUXSH/oh-my-winuxsh.winux" ] && . "$WINUXSH/oh-my-winuxsh.winux"
```

History sharing can be selected with `WINUXSH_HISTORY_MODE=shared`,
`session`, or `private`. See [Advanced usage](docs/src/advanced-usage.md)
for the behavior of each mode.

## Terminal toys

Winuxsh's terminal isn't just for commands — it prints pictures. The
[terminal-flags](https://github.com/caomengxuan666/terminal-flags) project
turns any image or GIF into a standalone ANSI printer script:

```sh
winuxsh flags/taffy.sh         # photos, right in the terminal
winuxsh flags/qiu-dance.sh     # animated GIFs, frame timing preserved
```

Truecolor half-block pixels, no Python or Pillow needed at runtime:

<img src="assets/demo-qiu-dance.gif" alt="An animated Qiubiaoqing sticker playing inside a Winuxsh terminal, printed from a generated shell script" width="560"/>

## Documentation

Full docs site: **[docs](https://unixwin.github.io/winuxsh/)** · [Getting started](docs/src/getting-started.md) · [Why Winuxsh](docs/src/why-winuxsh.md) · [Advanced usage](docs/src/advanced-usage.md) · [Architecture](docs/src/architecture.md)

Under the hood: [rubash](https://github.com/unixwin/rubash) (Bash engine) · WinuxCmd (Unix commands) · [reedline](https://github.com/nushell/reedline) (line editor)

## FAQ

- **Another Git Bash?** No — Git Bash emulates Unix on top of Windows. Winuxsh is a native Windows process: native paths, direct Windows binary execution, Bash compatibility in the language engine, not a fake filesystem.
- **Still need WSL?** Sure — for real Linux kernels, Linux Docker, Linux-only toolchains. For the other 95% of your day: you don't need WSL. You need Winuxsh.
- **Where's my config?** `~/.winuxshrc` — plain Bash. Winuxsh manages its own
  machine state; users do not need to maintain a second configuration format.

---

If Winuxsh just saved you from booting a Linux VM to run `grep`,
[star the repo](https://github.com/unixwin/winuxsh) and tell a Windows
developer. ★

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
