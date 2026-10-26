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
  `~/.niubashrc`, the generated plugin loader, prompt plugins, or interactive
  lifecycle hooks.
- Use `-C` only when a one-shot command needs the same startup state as the
  interactive REPL. It loads `~/.niubashrc` and lifecycle hooks, then exits.

This separation keeps automation deterministic while still allowing a rich
interactive shell.

### Verbatim base64 handoff: `--encoded-command`

When the command text cannot survive another shell's quoting (an agent
handoff, a CI matrix, a PowerShell launcher), pass it base64-encoded. The
payload is decoded and run through the `-c` route, so leading flags keep
their meaning (`-e -u -o pipefail --norc ...`) and words after the payload
are `$0` and positional parameters, exactly like `-c`:

```pwsh
niu --encoded-command 'cHdkOyBlY2hvIG9r'
niu --ec 'cHdkOyBlY2hvIG9r'          # short alias, same route
niu -EncodedCommand 'cHdkOyBlY2hvIG9r'  # PowerShell spelling
```

Three spellings work: `--encoded-command`, `--ec`, and `-EncodedCommand`
matched case-insensitively (niubash#474, #477). A lone `-ec` is not one of
them — with a single dash it stays GNU `-e -c`. An optional `v1:` prefix on
the payload guards against MSYS path rewriting on the way in. A missing
payload is an error, not an empty run.

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
expands to your home directory. If the file does not exist, it is silently
skipped and the command runs without it — the same behavior as GNU bash's
`BASH_ENV`.

With neither variable set, `-c` stays zero-load and fast.

## Startup And Config

Use `~/.niubashrc` as the normal human-authored entry point:

```bash
# Optional floor knobs — they only shape the built-in default prompt and
# completion UI; an enabled external theme claims PS1 and wins.
# NIU_PROMPT_CWD_STYLE='home'
# NIU_COMPLETION_STYLE='column'   # column (default) | gnu | ide | list | inline

alias ll='ls -la'
export EDITOR=vim

# starship (or any tool that sets PS1) owns the prompt while it is active:
# eval "$(starship init bash)"
```

Themes and plugins are enabled through `niu plugin`. Plugin verbs never write
the rc (niubash#359, v1.4.2): they update the manifest
(`~/.niubash/plugins.toml` + `~/.niubash/sources/registry.toml`) and
regenerate `~/.niubash/generated/plugins.niu`, which the host sources
read-only *before* your rc — so rc lines always win
(see [Plugin Workflow](#plugin-workflow)).

Interactive shells read ONE startup file: `~/.niubashrc`. By default it
is the only candidate: if it does not exist, the shell starts with no rc
at all — `~/.bashrc` is not read. The single exception is opt-in: if you
explicitly picked the `bashrc-fallback` rc policy during setup
(`niu setup --rc-policy bashrc-fallback`), a genuinely absent
`~/.niubashrc` lets `~/.bashrc` fill the slot instead (the migration path
for existing bash configurations). Either way the two never stack — one
config file, one blast radius, no implicit double-sourcing. Want the bash
layer under niu? Add one visible line to `~/.niubashrc`:

```bash
[[ -f ~/.bashrc ]] && source ~/.bashrc
```

Explicit, visible, yours — the same shape as bash's own
`.bash_profile` → `.bashrc` chain. `--norc` skips startup files;
`--rcfile <file>` reads only the named file, exactly like GNU bash. The
legacy `~/.winuxshrc` and `~/.winshrc` names are never read, migrated, or
written. Machine state —
the plugin manifest, source registry and trust records, the generated
loader, test isolation, completion directories — is maintained by Niubash
itself and is not a user configuration entry point.

## Script Debugging Flags

Two startup flags wire debugger support. Neither affects `-c` automation
unless you pass them explicitly.

### `--debug` — arm the DEBUG-trap substrate (niu extension)

`niu --debug script.sh` turns `extdebug` on from the first command: DEBUG
traps fire at every GNU stop site with `BASH_ARGC`/`BASH_ARGV` populated,
so a trap-based debugger or profiler can install itself without editing
the script. GNU bash accepts the same flag as a no-op (its internal
`debugging` variable is write-only — nothing in bash reads it), so scripts
that pass `--debug` keep running unchanged under GNU bash too.

### `--debugger` — run bashdb (GNU parity)

`niu --debugger script.sh` mirrors GNU bash: the shell looks for the bashdb
starter file (`bashdb-main.inc`) and sources it before your script runs.
Search order: `$BASHDB_START_FILE` (set this when your bashdb lives under a
nonstandard prefix — Termux, a custom `--prefix` build), then
`/usr/local/share/bashdb/bashdb-main.inc`, then
`/usr/share/bashdb/bashdb-main.inc`. If no starter file exists, niubash
prints GNU's diagnostic pair (`No such file or directory` + `warning:
cannot start debugger; debugging mode disabled`) and continues with
debugging disabled — exactly what GNU bash does. With bashdb installed,
existing bashdb users point the shell at their starter and upgrade in
place.

Do not put automation-critical behavior only in an interactive rc file. Pass
needed environment variables directly to `niu -c` or the script process, or
use `NIU_ENV` / `BASH_ENV` to source a dedicated one-shot init file (see
[One-shot init file](#one-shot-init-file-niu_env--bash_env)).

## Completion Styles

TAB completion comes in five shapes. The default since 1.4.0 is `column`
(the GNU listing look, without GNU's two-TAB listing dance); before
that it was `ide`. The literal GNU readline behavior is the opt-in `gnu`
style below. Pick one with `niu config set completion.style <name>`
(persisted in `~/.niubash/config.toml`) or `NIU_COMPLETION_STYLE=<name>`
in `~/.niubashrc` or the environment — the env variable wins over the
persisted value:

- `gnu` — GNU readline: TAB completes the longest common prefix
  (a lone candidate completes outright); pressing TAB again on an unchanged
  line prints the candidates as width-adaptive columns next to the prompt
  and repaints it. The list is printed output, not a menu: nothing is
  selected, arrow keys stay on history, no popup.
- `ide` — multi-column popup with descriptions, VS Code style. Column count
  adapts to the terminal width; the description of the selected candidate
  renders beside it.
- `column` (default) — plain multi-column grid (like zsh `compinit`),
  drawn by reedline's `ColumnarMenu`. The menu recomputes its layout on
  every draw, and the presentation follows the candidate list: when any
  candidate in the list carries a description (e.g. flag completions
  such as `git checkout -<TAB>`), the whole list is presented as a
  single vertical column, each description beside its value. That
  single-column presentation is by design (owner ruling 2026-10-26):
  it keeps the descriptions readable instead of squeezing them into
  grid cells. When no candidate carries a description, the list is
  presented as a grid of as many columns as the terminal width fits,
  down to one column when the width fits only one.
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

A second compiled layer covers the common external commands that are not
WinuxCmd applets — git, tar, curl, ssh, docker, kubectl, and npm (issue
#305): static subcommand/flag tables under
`crates/niubash-runtime/assets/completions/common/`, so `git co<Tab>` offers
`checkout`/`commit`/`config` and `kubectl get -<Tab>` offers `-o`/
`--all-namespaces` on a fresh install.

Priority is layered: the embedded applet and common-command defaults are the
base layer, and completion definitions loaded later override them per
command — a bundle/pack TOML or a user completion-dir TOML whose file stem
(or `command` key) matches the built-in name replaces the built-in
definition entirely, so trimming or extending a command's flags is a plain
file drop, not a fight with the defaults.

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

A `NIU_COMPDEFS` line can also name a directory (issue #306): every line
that resolves to an existing directory is loaded as a completion-definition
directory — `<cmd>.toml` files in the same schema as the embedded applet
definitions, and `<cmd>.bash` standard bash completion scripts (read as
data, never executed). Remaining lines are `command:function` pairs:

```bash
NIU_COMPDEFS="C:/agents/completions"
NIU_COMPDEFS="$NIU_COMPDEFS
git:niu_git_comp"
```

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

Enablement is per source or per asset (a theme), and it lands in the
generated loader, not your rc:

```sh
niu plugin enable oh-my-bash   # records the source; republishes ~/.niubash/generated/plugins.niu
niu plugin enable agnoster     # a theme asset -> sets OSH_THEME in the loader
niu plugin disable oh-my-bash  # manifest updated; the built-in floor returns
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

Elevation is a three-layer delegation (niubash#429): Niubash never
elevates by itself — one of the layers below owns UAC, process creation,
environment forwarding, and console handling.

1. **The Rubash `sudo` builtin — off by default, opt-in only.** The
   embedded engine's experimental elevation builtin stays disabled
   unless `NIU_ENABLE_RUBASH_SUDO=1` is set before Niubash starts (or a
   host runs `enable sudo` with an elevation handler installed). It
   exists for host integrators and is not the recommended Windows path.

2. **Microsoft sudo — the in-box channel on current Windows.** Windows
   ships `sudo.exe` from build 26045 onward (generally available with
   Windows 11 24H2); enable it under Settings → System → For
   developers → Sudo. Where present it is the preferred provider:
   `sudo your-command` works with no extra install.

3. **gsudo — the package channel, and the only one on Windows 10.**
   The WPM `gsudo` package covers the machines Microsoft sudo never
   reaches:

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

`niu doctor` reports which channel a machine actually has (the
`elevation channel` row), so a missing provider is visible before a
script depends on it.

Do not alias `sudo` automatically in shared scripts. A user who wants the
Unix spelling interactively can add this to `~/.niubashrc`:

```bash
alias sudo='gsudo'
```

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

### `$HOME` stays in Windows form (niubash#522)

`$HOME` is deliberately the Windows-native form (`C:/Users/you`), not
the POSIX-persona form (`/c/Users/you`) that `$PWD` renders in. The two
spellings name the same directory, so inside your home directory
`test "$HOME" = "$PWD"` is false — that inequality is expected, not a
bug.

The Windows form is a contract, not an oversight: `$HOME` is exported
to every child process, and native Windows tools that receive it
(editors, Git, language runtimes, anything outside the shell) expect a
path they can open directly. The `/c/...` persona is a shell-side
compatibility spelling for input and display; most other programs
cannot resolve it.

Scripts that need the POSIX form of home convert explicitly, at the
point of use:

```bash
# $HOME (C:/Users/you) -> persona form (/c/Users/you)
drive=$(printf '%s' "${HOME:0:1}" | tr '[:upper:]' '[:lower:]')
home_posix="/$drive${HOME:2}"

# and back: /c/Users/you -> c:/Users/you (drive-letter case is cosmetic
# to Windows APIs)
home_win="${home_posix:1:1}:${home_posix:2}"
```

If Git for Windows is installed, `cygpath -u "$HOME"` and
`cygpath -w "$home_posix"` do the same conversions. Conversely, when
handing a path to a native tool, pass `$HOME`-style Windows forms
as-is — do not pre-convert them to the persona spelling.

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
