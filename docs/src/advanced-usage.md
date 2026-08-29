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
niubash -C 'alias ll; pwd'       # one-shot REPL command
```

- Use the interactive REPL for normal shell work.
- Use `-c` for scripts, tests, CI, and coding agents. It does not load
  `~/.niubashrc`, `~/.winshrc`, prompt plugins, or interactive lifecycle hooks.
- Use `-C` only when a one-shot command needs the same startup state as the
  interactive REPL. It loads `~/.niubashrc` and lifecycle hooks, then exits.

This separation keeps automation deterministic while still allowing a rich
interactive shell.

## Startup And Config

Use `~/.niubashrc` as the normal human-authored entry point:

```bash
NIU_THEME=p10-classic
NIU_THEME_PLUGIN=theme-p10-classic
NIU_PROMPT_SYMBOL=">"
export NIU_THEME NIU_THEME_PLUGIN NIU_PROMPT_SYMBOL

NIU_PLUGINS=(prompt-core git common-aliases path-tools extract)

[ -f "$NIUBASH/oh-my-winuxsh.winux" ] && . "$NIUBASH/oh-my-winuxsh.winux"

alias ll='ls -la'
export EDITOR=vim
```

The legacy files still exist, but they should not be the primary user path:

- `~/.winshrc` is a fallback only when `~/.niubashrc` is absent.
- Plugin CLI records, migration blocks, bundle versions, tests, and advanced
  overrides are internal managed state, not a user configuration file.

Do not put automation-critical behavior only in an interactive rc file. Pass
needed environment variables directly to `niu -c` or the script process.

## Prompt And Themes

Prompt behavior is plugin-owned. The core shell provides host APIs and
lifecycle hooks; official theme and prompt behavior lives in bundled plugins.

Common rc shape:

```bash
NIU_THEME=p10-lean
NIU_THEME_PLUGIN=theme-p10-lean
NIU_PLUGINS=(prompt-core git)
export NIU_THEME NIU_THEME_PLUGIN

[ -f "$NIUBASH/oh-my-winuxsh.winux" ] && . "$NIUBASH/oh-my-winuxsh.winux"
niubash_prompt_use_template "{cwd} {git_prompt}{prompt_char} " "{status}{time} " 2>/dev/null || true
```

Theme assets can use named colors, 256-color indexes, and true-color
`#RRGGBB` foreground/background values. Prefer changing the theme plugin or
theme asset instead of hardcoding prompt rendering in shell core.

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

Use the CLI to inspect the active bundle instead of relying on stale docs:

```sh
niu plugin list
niu plugin search git
niu plugin themes
niu plugin info git
niu plugin review git
niu plugin doctor
```

Normal interactive choices belong in `~/.niubashrc`:

```bash
NIU_PLUGINS=(prompt-core git docker kubectl zoxide)
NIU_THEME_PLUGIN=theme-p10-rainbow
```

Use managed plugin CLI operations when you need a reviewable machine record,
permissions, bundle update state, or rollback.

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
niubash --self-update --check
niubash --self-update

winuxcmd.exe wpm update winuxcmd

niu plugin update oh-my-winuxsh --github-release latest
niu plugin rollback oh-my-winuxsh
```

- `niubash --self-update` updates the shell.
- `wpm update winuxcmd` updates command packages and command links.
- `plugin update oh-my-winuxsh` updates the official plugin bundle.

## Debug Checklist

For shell issues, capture the active binary and execution path first:

```bash
niubash --version
command -v niubash
command -v winuxcmd.exe
echo "$SHELL"
niu -c 'echo command-mode:$SHELL'
niubash -C 'echo repl-command:$SHELL'
```

For repository changes, run focused tests before broad suites:

```bash
cargo test --test repl_command --locked
cargo test -p niubash-runtime --lib --locked
cargo test --test plugin_inventory --locked
```

Use [Plugin System Direction](../planning/plugin-system-direction.md) for architecture and
[Plugin System Roadmap](../planning/plugin-system-roadmap.md) for execution order.
