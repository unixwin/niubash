# Advanced Winuxsh Usage

This guide covers the surfaces that matter after the first successful launch:
execution modes, startup files, prompt/theme plugins, command discovery, and
update/debug workflows.

For first-time setup, start with [Getting Started](getting-started.md).

## Execution Modes

Winuxsh has three intentionally different execution paths:

```pwsh
winuxsh                         # interactive REPL
winuxsh -c 'pwd; echo "$SHELL"'  # quiet script/CI command mode
winuxsh -C 'alias ll; pwd'       # one-shot REPL command
```

- Use the interactive REPL for normal shell work.
- Use `-c` for scripts, tests, CI, and coding agents. It does not load
  `~/.winuxshrc`, `~/.winshrc`, prompt plugins, or interactive lifecycle hooks.
- Use `-C` only when a one-shot command needs the same startup state as the
  interactive REPL. It loads `~/.winuxshrc` and lifecycle hooks, then exits.

This separation keeps automation deterministic while still allowing a rich
interactive shell.

## Startup And Config

Use `~/.winuxshrc` as the normal human-authored entry point:

```bash
WINUXSH_THEME=p10-classic
WINUXSH_THEME_PLUGIN=theme-p10-classic
WINUXSH_PROMPT_SYMBOL=">"
export WINUXSH_THEME WINUXSH_THEME_PLUGIN WINUXSH_PROMPT_SYMBOL

WINUXSH_PLUGINS=(prompt-core git common-aliases path-tools extract)

[ -f "$WINUXSH/oh-my-winuxsh.winux" ] && . "$WINUXSH/oh-my-winuxsh.winux"

alias ll='ls -la'
export EDITOR=vim
```

The legacy files still exist, but they should not be the primary user path:

- `~/.winshrc` is a fallback only when `~/.winuxshrc` is absent.
- Plugin CLI records, migration blocks, bundle versions, tests, and advanced
  overrides are internal managed state, not a user configuration file.

Do not put automation-critical behavior only in an interactive rc file. Pass
needed environment variables directly to `winuxsh -c` or the script process.

## Prompt And Themes

Prompt behavior is plugin-owned. The core shell provides host APIs and
lifecycle hooks; official theme and prompt behavior lives in bundled plugins.

Common rc shape:

```bash
WINUXSH_THEME=p10-lean
WINUXSH_THEME_PLUGIN=theme-p10-lean
WINUXSH_PLUGINS=(prompt-core git)
export WINUXSH_THEME WINUXSH_THEME_PLUGIN

[ -f "$WINUXSH/oh-my-winuxsh.winux" ] && . "$WINUXSH/oh-my-winuxsh.winux"
winuxsh_prompt_use_template "{cwd} {git_prompt}{prompt_char} " "{status}{time} " 2>/dev/null || true
```

Theme assets can use named colors, 256-color indexes, and true-color
`#RRGGBB` foreground/background values. Prefer changing the theme plugin or
theme asset instead of hardcoding prompt rendering in shell core.

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
winuxsh plugin list
winuxsh plugin search git
winuxsh plugin themes
winuxsh plugin info git
winuxsh plugin review git
winuxsh plugin doctor
```

Normal interactive choices belong in `~/.winuxshrc`:

```bash
WINUXSH_PLUGINS=(prompt-core git docker kubectl zoxide)
WINUXSH_THEME_PLUGIN=theme-p10-rainbow
```

Use managed plugin CLI operations when you need a reviewable machine record,
permissions, bundle update state, or rollback.

## Command Discovery And WPM

Winuxsh resolves Unix-style commands through normal Windows `PATH`. When a
command is missing or comes from the wrong provider, inspect the active
installation:

```bash
command -v winuxsh
command -v winuxcmd.exe
command -v ls
winuxcmd.exe wpm index status
winuxcmd.exe wpm search jq
winuxcmd.exe wpm links rebuild --force
```

Do not assume `/usr/bin` exists. Winuxsh is a Windows process using Windows
executables and command links.

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
winuxsh --self-update --check
winuxsh --self-update

winuxcmd.exe wpm update winuxcmd

winuxsh plugin update oh-my-winuxsh --github-release latest
winuxsh plugin rollback oh-my-winuxsh
```

- `winuxsh --self-update` updates the shell.
- `wpm update winuxcmd` updates command packages and command links.
- `plugin update oh-my-winuxsh` updates the official plugin bundle.

## Debug Checklist

For shell issues, capture the active binary and execution path first:

```bash
winuxsh --version
command -v winuxsh
command -v winuxcmd.exe
echo "$SHELL"
winuxsh -c 'echo command-mode:$SHELL'
winuxsh -C 'echo repl-command:$SHELL'
```

For repository changes, run focused tests before broad suites:

```bash
cargo test --test repl_command --locked
cargo test -p winuxsh-runtime --lib --locked
cargo test --test plugin_inventory --locked
```

Use [Plugin System Direction](../planning/plugin-system-direction.md) for architecture and
[Plugin System Roadmap](../planning/plugin-system-roadmap.md) for execution order.
