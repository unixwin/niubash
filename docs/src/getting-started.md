# Getting Started with Niubash

A short walkthrough from zero to a working prompt with git status.

## 1. Build or download

```sh
git clone https://github.com/unixwin/niubash.git
cd niubash
cargo build --release
```

After building, the binary is at `target\release\niu.exe`. You can run it
directly, or add `target\release` to your user `PATH` using your normal Windows
environment settings:

```sh
target/release/niu.exe
```

If you are using the release zip, niubash automatically runs the activation
script on first start when command links are missing:

```bash
niubash winuxcmd/activate-winuxcmd.sh
```

That creates local command links inside `winuxcmd/`, so `ls`, `cat`, and
friends resolve normally. Once the links exist, startup skips activation.

## 2. Start the shell

```sh
niubash
```

You should see something like:

```text
user@DESKTOP C:\Users\you
%
```

Type `exit` or press Ctrl+D to quit.

## 3. See the git prompt

`cd` into any git repository:

```sh
cd C:\Users\you\repo
# if inside a repo, the prompt changes:
user@DESKTOP C:\Users\you\repo  git:(main) ●1 ✚2 ?1
%
```

Symbols at a glance:

| Symbol | Meaning |
|--------|---------|
| `●N`   | N files staged for commit |
| `✚N`   | N files modified but unstaged |
| `?N`   | N untracked files |
| `↑N`   | N commits ahead of upstream |
| `↓N`   | N commits behind upstream |
| `⚑N`   | N stashes saved |
| `✖N`   | N merge conflicts |

The branch name is green when the tree is clean, yellow when dirty.

## 4. Try some commands

```bash
pwd                                  # prints C:/Users/you/repo
ls -la                               # Unix-style listing
echo "hello from $USER"
for i in 1 2 3; do echo $i; done
if [ -f Cargo.toml ]; then echo "yep"; fi
cat Cargo.toml | grep name
grep -n "fn main" src/main.rs
```

Windows paths work directly:

```bash
ls C:\Windows\System32\drivers\etc
ls D:/Projects
cd "C:\Program Files"
```

Multiline blocks work naturally:

```bash
for f in *.toml; do
  echo "found $f"
done
```

## 5. Try git completions

```bash
git ad<Tab>                # completes to `git add`
git commit -<Tab>           # shows flags: --message, --all, --amend
git push --fo<Tab>          # completes to --force
git branch -<Tab>           # shows -d, -D, -m, -v, -a, -r
```

## 6. Set up your config

Create `~/.niubashrc` for interactive shell code, plugin selection, and theme
selection:

```bash
NIU_THEME=minimal
NIU_THEME_PLUGIN=theme-minimal
NIU_PROMPT_SYMBOL="❯"
export NIU_THEME NIU_THEME_PLUGIN NIU_PROMPT_SYMBOL

NIU_PLUGINS=(prompt-core git)

if [ -z "${HOME:-}" ] && [ -n "${USERPROFILE:-}" ]; then
  HOME="$USERPROFILE"
  export HOME
fi

if [ -z "${NIUBASH:-}" ]; then
  NIUBASH="$HOME/.oh-my-winuxsh"
  export NIUBASH
fi

[ -f "$NIUBASH/oh-my-winuxsh.winux" ] && . "$NIUBASH/oh-my-winuxsh.winux"
niubash_prompt_use_template "{cwd} {git_prompt}{prompt_char} " "{time} " 2>/dev/null || true

export EDITOR=vim
alias ll='ls -la'
alias la='ls -a'
alias gst='git status'
alias gco='git checkout'
alias gl='git log --oneline --graph --decorate --all'

hello() {
  echo "hello from niubash"
}
```

`~/.niubashrc` is sourced only for the interactive REPL and the `-C`
one-shot REPL command path. It does not run for `niu -c ...`, script files,
or stdin script execution, so agent and CI surfaces stay deterministic.

`~/.winshrc` is a legacy compatibility fallback and is used only when
`~/.niubashrc` is absent. Plugin CLI enable/disable records, migration blocks,
completion overrides, test isolation, and advanced machine state are managed
internally. Prefer `~/.niubashrc` for normal interactive customization.

## 6b. Prompt and theme plugins

Themes are official plugins. To use a Powerlevel-style theme, switch the theme
plugin in `~/.niubashrc`:

```bash
NIU_THEME=p10-lean
NIU_THEME_PLUGIN=theme-p10-lean
NIU_PLUGINS=(prompt-core git)
```

Useful bundled theme plugins include `theme-minimal`, `theme-classic`,
`theme-pure`, `theme-robbyrussell`, `theme-p10-lean`, `theme-p10-classic`,
`theme-p10-rainbow`, and `theme-p10-pure`. Theme assets support named
colours, 256-colour indexes, and true-colour `#RRGGBB` foreground/background
values plus bold, italic, underline, and dimmed flags.

Prompt templates use the public prompt-core API:

```bash
niubash_prompt_use_template "{cwd} {git}{prompt_char} " "{status}{time} "
```

Available template tokens include `{cwd}`, `{cwd_base}`, `{user_host}`,
`{git}`, `{git_prompt}`, `{status}`, `{time}`, `{command_execution_time}`,
`{newline}`, and `{prompt_char}`. The Git prompt snapshot is refreshed during
startup/precmd so late Git work warms the next prompt instead of redrawing the
line the user is typing on.

## 7. Official plugin bundle

Niubash has a built-in plugin system. `oh-my-winuxsh` is the
official bundled plugin distribution. It ships first-party packs such as `git`, `docker`, `kubectl`,
`npm`, `zoxide`, `direnv`, `dotenv`, `fzf`, prompt presets, and keybinding
presets.

The normal interactive shape is the `~/.niubashrc` plugin list shown above.
`niu plugin enable/disable` and migration tooling update internal managed
state. Official shell helper packs can
ship reviewed bundle-local `init.winux` source scripts. If `~/.niubashrc`
exists, it is the source-plugin entry point and loads the framework directly.
Without `~/.niubashrc`, managed startup can still load enabled source packs
before fallback `~/.winshrc`. Use `niu plugin list`,
`niu plugin search`, `niu plugin themes`, and
`niu plugin review` for current inventory, theme sources, and permission
checks.

## What next

- [Plugin System Direction](../planning/plugin-system-direction.md) for the v3 plugin model
- [Plugin System Roadmap](../planning/plugin-system-roadmap.md) for the execution sequence
- [Oh My Niubash Bundle Plan](../planning/oh-my-winuxsh-bundle-plan.md) for the official bundle
- [Roadmap](niubash-roadmap.md) to see what is planned
- Source at [github.com/unixwin/niubash](https://github.com/unixwin/niubash)
