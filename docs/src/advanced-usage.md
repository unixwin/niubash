# Advanced Niubash Usage

This guide covers the surfaces that matter after the first successful launch:
execution modes, startup files, prompt/theme plugins, command discovery, and
update/debug workflows.

For first-time setup, start with [Getting Started](getting-started.md).

## Execution Modes

Niubash has three intentionally different execution paths:

```pwsh
niubash                         # interactive REPL
niu -c 'pwd; echo "$SHELL"'  # quiet script/CI command mode
niu -C 'alias ll; pwd'       # one-shot REPL command
```

- Use the interactive REPL for normal shell work.
- Use `-c` for scripts, tests, CI, and coding agents. It does not load
  `~/.niubashrc`, `~/.winshrc`, prompt plugins, or interactive lifecycle hooks.
- Use `-C` only when a one-shot command needs the same startup state as the
  interactive REPL. It loads `~/.niubashrc` and lifecycle hooks, then exits.

This separation keeps automation deterministic while still allowing a rich
interactive shell.

### One-shot init file: `NIU_ENV` / `BASH_ENV`

By default `-c` loads nothing — no rc, no plugins, no hooks. If your script,
CI job, or coding agent needs shell aliases, exported variables, or PATH
tweaks, point `NIU_ENV` (or bash-compatible `BASH_ENV`) at a dedicated init
file. Only that one file is sourced, so the interactive-only content in
`~/.niubashrc` stays out of the one-shot path.

```bash
# ~/.opencode.env — a minimal init file for agents
export PATH="$HOME/tools:$PATH"
alias ll='ls -la'
export DOCKER_CONTEXT=my-cluster
```

```bash
NIU_ENV=~/.opencode.env niu -c 'll | head'
# or bash-compatible:
BASH_ENV=~/.opencode.env niu -c 'echo "$DOCKER_CONTEXT"'
```

`NIU_ENV` takes precedence over `BASH_ENV` when both are set. A leading `~`
expands to your home directory. If the file does not exist, niubash prints
an error to stderr and continues — matching GNU bash's `BASH_ENV` behavior.

With neither variable set, `-c` stays zero-load and fast.

## Startup And Config

Use `~/.niubashrc` as the normal human-authored entry point:

```bash
# Optional floor knobs — they only shape the built-in default prompt and
# completion UI; an enabled external theme claims PS1 and wins.
# NIU_PROMPT_CWD_STYLE='home'
# NIU_COMPLETION_STYLE='gnu'   # gnu | ide | column | list | inline

alias ll='ls -la'
export EDITOR=vim

# starship (or any tool that sets PS1) owns the prompt while it is active:
# eval "$(starship init bash)"
```

Themes and plugins are enabled through `niu plugin`, which appends managed
blocks at the end of the rc (see [Plugin Workflow](#plugin-workflow)); your
lines outside those blocks are never rewritten.

The legacy files still exist, but they should not be the primary user path:

- `~/.winshrc` is a fallback only when `~/.niubashrc` is absent.
- Plugin CLI records, migration blocks, bundle versions, tests, and advanced
  overrides are internal managed state, not a user configuration file.

Do not put automation-critical behavior only in an interactive rc file. Pass
needed environment variables directly to `niu -c` or the script process, or
use `NIU_ENV` / `BASH_ENV` to source a dedicated one-shot init file (see
[One-shot init file](#one-shot-init-file-niu_env--bash_env)).

## Completion Styles

TAB completion comes in five shapes. The default since 1.4.1 is `gnu`
(GNU readline factory behavior); before that it was `ide`. Pick one with
`niu config set completion.style <name>` (persisted in
`~/.niubash/config.toml`) or `NIU_COMPLETION_STYLE=<name>` in
`~/.niubashrc` or the environment — the env variable wins over the
persisted value:

- `gnu` (default) — GNU readline: TAB completes the longest common prefix
  (a lone candidate completes outright); pressing TAB again on an unchanged
  line prints the candidates as width-adaptive columns next to the prompt
  and repaints it. The list is printed output, not a menu: nothing is
  selected, arrow keys stay on history, no popup.
- `ide` — multi-column popup with descriptions, VS Code style. Column count
  adapts to the terminal width; the description of the selected candidate
  renders beside it.
- `column` — plain multi-column grid (like zsh `compinit`).
- `list` — vertical list with descriptions (like fish).
- `inline` — first match inserted directly, Tab cycles (like bash
  `menu-complete`).

`show-all-if-ambiguous` / `show-all-if-unmodified`-style readline variables
are not implemented yet; the double-TAB listing is the fixed shape.

Flag completions (`ls --a<Tab>` → `--all` with its description) come from the
bundle's generated completion assets and work in every style; `ide` and
`list` show the descriptions inline.

## WinuxCmd Applet Completions

Every WinuxCmd applet ships with flag completion built into the `niu` binary:
`grep --col<Tab>` offers `--color` / `--colour` with descriptions,
`ls --color <Tab>` offers `always` / `auto` / `never`, and `wpm <Tab>`
lists the package-manager subcommands (`install`, `search`, `uninstall`,
...).

These definitions are generated from `winuxcmd --help` (WinuxCmd 1.1.5
transcripts) by `scripts/generate-winuxcmd-completions.py`, stored as one
TOML per applet under `crates/niubash-runtime/assets/completions/winuxcmd/`,
and compiled into the runtime — so they work on a fresh install with no
plugin enabled. When WinuxCmd gains applets or options, regenerate against
the new binary:

```bash
python scripts/generate-winuxcmd-completions.py --winuxcmd <path-to-winuxcmd.exe>
python scripts/test-winuxcmd-completions.py   # golden check: corpus vs committed assets
```

Priority is layered: the embedded applet defaults are the base layer, and
completion definitions loaded later override them per command — a
bundle/pack TOML or a user completion-dir TOML whose file stem (or `command`
key) matches the applet name replaces the built-in definition entirely, so
trimming or extending an applet's flags is a plain file drop, not a fight
with the defaults.

## AI Agent Skill Bundle

For AI hosts (Claude, ZCode, Cursor, ...) that drive your shell, niubash
ships a skill bundle describing its capability surface and dialect
contract: `skills/niubash/SKILL.md` plus `references/`. Install it without
hunting for the zip:

```bash
niu skill install                       # default: ~/.claude/skills/niubash/
niu skill install --target all          # claude + zcode + cursor
niu skill install --target C:/agents/skills   # generic: <dir>/niubash/
niu skill status                        # current / outdated per target (byte compare + sha256)
```

`niu doctor` reports an advisory `agent skill` row. The released
`niubash-skill-v*.zip` contains the same tree (WinuxCmd-skill layout), so
unpacking the zip into an agent skills directory is equivalent.

The command tables inside the bundle are generated — hand-written lists
rot. `scripts/generate-skill.py` fills the GENERATED-marked regions of
`SKILL.md` and `references/quickref.md` from the engine's own surfaces:
a captured `help -s '*'` transcript (builtin table), a captured
`niu --help` transcript (launcher/plugin verbs), and the applet completion
inventory (180 winuxcmd applets). After changing the launcher help or the
builtin table, refresh and re-check:

```bash
cargo build && python scripts/generate-skill.py --capture --niu target/debug/niu.exe
python scripts/test-skill-bundle.py   # golden check: regions + embed manifest
```

## Prompt And Themes

Defaults are a floor, not an identity. The built-in prompt (a reedline
template: `user@host cwd symbol`) renders only while nothing claims `PS1`.
Whoever sets `PS1` last owns the prompt completely — your own rc line, an
enabled oh-my-bash theme, or starship:

```bash
PS1='\u@\h \w \$ '                # your own prompt: simplest possible claim
eval "$(starship init bash)"      # starship claims PS1 on its first prompt
```

Disabling the claim (`niu plugin disable oh-my-bash`, or a plain
`unset PS1`) lets the built-in floor render again. `PROMPT_COMMAND` alone
is not a claim — it is a pre-prompt hook; hooks that want the prompt set
`PS1` themselves.

The floor itself is shaped by `NIU_PROMPT_CWD_STYLE` (`home`, `full`, or
`basename`; see [Getting Started](getting-started.md)). External themes
style themselves through the bash-compatible `PS1` channel — colors,
powerline glyphs, and git segments come from the theme, not the shell.

## History Modes

Set `NIU_HISTORY_MODE` in `~/.niubashrc` when multiple shells share a
history file:

```bash
NIU_HISTORY_MODE=private
export NIU_HISTORY_MODE
```

- `shared` (default) refreshes navigation from other shells.
- `session` keeps the startup snapshot stable for navigation while builtins can
  observe later file updates.
- `private` loads the complete history file at startup, then keeps later
  navigation changes local to the current shell while appending its own commands.

## Custom Key Widgets

A shell function can act as a line editor widget. Declare it in
`~/.niubashrc` with `NIU_BINDKEYS` (one `key:widget` entry per line):

```bash
niu_fzf_file() {
    local file
    file="$(fd -t f | fzf)" || return 0
    NIU_WIDGET_RESULT="ni $file"
    NIU_WIDGET_ACCEPT=1
}
NIU_BINDKEYS="Ctrl+X:niu_fzf_file"
```

When the key fires, the function runs with the current editor state:

- In: `NIU_WIDGET_BUFFER` (current buffer) and `NIU_WIDGET_CURSOR` (byte
  offset). Both are temporary and restored afterwards.
- Out: `NIU_WIDGET_RESULT` replaces the buffer (unset or absent keeps it;
  an empty string clears the line), `NIU_WIDGET_CURSOR_RESULT` moves the
  cursor to a byte offset, and `NIU_WIDGET_ACCEPT=1` submits the buffer as
  if Enter had been pressed.

Keys are single-key sequences such as `Ctrl+X`, `Alt+G`, a plain character,
or escape forms like `^X`. The function runs as ordinary shell code: aliases,
PATH, and your rc all apply. Unknown widget names in bundle bindkeys follow
the same contract, so plugins can ship function widgets too.

## Shell-Function Completions

A shell function can provide completions for a command's arguments. Declare
it in `~/.niubashrc` with `NIU_COMPDEFS` (one `command:function` entry per
line):

```bash
niu_git_comp() {
    local branches
    branches="$(git branch --format='%(refname:short)')" || return 0
    NIU_COMP_RESULT="$branches"
}
NIU_COMPDEFS="git:niu_git_comp"
```

When completing arguments for a registered command, the function runs with:

- In: `NIU_COMP_WORDS` (space-joined words, matching bash `COMP_WORDS`) and
  `NIU_COMP_CWORD` (bash `COMP_CWORD` semantics). Both are temporary.
- Out: `NIU_COMP_RESULT`, one candidate per line, shaped `value` or
  `value<TAB>description`.

Compdef functions run synchronously during completion, while the line editor
owns the terminal. They must not print to stdout: compute with command
substitution and write `NIU_COMP_RESULT` instead. Bundle keybindings TOML and
`NIU_BINDKEYS` share the same widget vocabulary, so plugins and users can
compose the two contracts freely.

## Git Prompt Performance

Git status should be consumed as a coherent prompt snapshot, not rendered by
blocking every prompt draw with fresh Git processes. The intended shape is:

- prompt/theme plugins render the latest available snapshot;
- the host keeps git status work warm in the background;
- late git work updates the next prompt instead of repainting the active input
  line.

If the prompt flickers or repaints the current line, debug the lifecycle and
git snapshot path rather than adding more inline Git calls to the theme.

## Plugin Workflow

Use the CLI to inspect the ecosystem instead of relying on stale docs:

```sh
niu plugin list          # sources, their assets, activation state
niu plugin discover      # read-only overview, including not-yet-installed managers
```

Enablement is per source or per asset (a theme), and it edits your rc, not
an internal database:

```sh
niu plugin enable oh-my-bash   # guarded loader block appended to ~/.niubashrc
niu plugin enable agnoster     # a theme asset -> sets OSH_THEME in that block
niu plugin disable oh-my-bash  # removes the block; the built-in floor returns
```

`niu plugin update|sync|restore|clean` are the lockfile-style maintenance
verbs for installed sources.

## Third-Party Sources

Beyond the curated catalog (`oh-my-bash`, `bash-it`, `bash-completion`),
niu can install any git repository or local path that follows a known
plugin-manager layout. New sources are **untrusted by default**: nothing
they ship runs until you explicitly trust them.

```sh
niu plugin add https://github.com/someone/oh-my-bash-fork.git
# review the cloned source under ~/.niubash/sources/<id>
niu plugin trust oh-my-bash-fork
niu plugin enable oh-my-bash-fork
niu plugin source remove oh-my-bash-fork   # delete tree + registry entry
```

Notes:

- Sources are cloned to `~/.niubash/sources/<id>` and registered in
  `~/.niubash/sources/registry.toml`; the registry records the origin, ref,
  path, and trust state.
- `niu plugin update <id>` re-fetches the registered origin;
  `niu plugin rollback <id>` returns to the previous version.
- An untrusted source never activates: enable refuses until
  `niu plugin trust <id>` passes the review gate.
- A git ref can be pinned at add time with `niu plugin add <id> --ref <ref>`.
- `NIU_PLUGIN_SOURCES_ROOT` overrides the sources root (portable setups and
  tests).

## Command Discovery And WPM

Niubash resolves Unix-style commands through normal Windows `PATH`. When a
command is missing or comes from the wrong provider, inspect the active
installation:

```bash
command -v niubash
command -v winuxcmd.exe
command -v ls
winuxcmd.exe wpm index status
winuxcmd.exe wpm search jq
winuxcmd.exe wpm links rebuild --force
```

Do not assume `/usr/bin` exists. Niubash is a Windows process using Windows
executables and command links.

## Elevated Commands

Niubash disables Rubash's experimental `sudo` builtin by default. Windows
elevation is delegated to the WPM `gsudo` package, which owns UAC, process
creation, environment forwarding, and console handling.

```bash
command -v gsudo
gsudo --version
gsudo your-command args
```

Install or repair it through the active WPM provider when necessary:

```bash
wpm search gsudo
wpm install gsudo
wpm links rebuild --force
```

Do not alias `sudo` automatically in shared scripts. A user who wants the
Unix spelling interactively can add this to `~/.niubashrc`:

```bash
alias sudo='gsudo'
```

The embedded Rubash elevation builtin remains available only for host
integrators. Set `NIU_ENABLE_RUBASH_SUDO=1` before starting Niubash, or run
`enable sudo` in a shell that supports an elevation handler. This is not the
recommended Windows path.

## Windows Paths And Home

Prefer durable Windows paths in scripts:

```bash
cd C:/Users/you/repo
ls "C:\Program Files"
cd ~
```

Prompt display should normally render the home directory as `~` and descendants
as `~/path`, but internal process paths remain native Windows paths. Treat
`/c/Users/...` as compatibility input, not the primary model.

## Updating

Keep the three update planes separate:

```bash
niu --self-update --check
niu --self-update

winuxcmd.exe wpm update winuxcmd

niu plugin update oh-my-bash
niu plugin rollback oh-my-bash
```

- `niu --self-update` updates the shell.
- `wpm update winuxcmd` updates command packages and command links.
- `niu plugin update <source-id>` updates an installed plugin source;
  `rollback` returns it to the previous version.

## Debug Checklist

For shell issues, capture the active binary and execution path first:

```bash
niu --version
command -v niubash
command -v winuxcmd.exe
echo "$SHELL"
niu -c 'echo command-mode:$SHELL'
niu -C 'echo repl-command:$SHELL'
```

For repository changes, run focused tests before broad suites:

```bash
cargo test --test repl_command --locked
cargo test -p niubash-runtime --lib --locked
cargo test --test plugin_inventory --locked
```

Use [Plugin System Direction](../planning/plugin-system-direction.md) for architecture and
[Plugin System Roadmap](../planning/plugin-system-roadmap.md) for execution order.
