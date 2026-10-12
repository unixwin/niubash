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
niu winuxcmd/activate-winuxcmd.sh
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

Create `~/.niubashrc` for interactive shell code — environment, aliases,
functions — plus optional defaults and theme enablement:

```bash
# Optional floor knobs: they only shape the built-in default prompt and
# completion UI. An external theme claims PS1 and overrides the floor.
# NIU_PROMPT_CWD_STYLE='home'      # home | full | basename
# NIU_COMPLETION_STYLE='gnu'       # gnu (default) | ide | column | list | inline

if [ -z "${HOME:-}" ] && [ -n "${USERPROFILE:-}" ]; then
  HOME="$USERPROFILE"
  export HOME
fi

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

## 6b. Prompts and themes

The built-in prompt is a floor, not an identity: it renders only while
nothing claims `PS1`. Anything that sets `PS1` — your own line in
`~/.niubashrc`, an enabled oh-my-bash theme, or `eval "$(starship init bash)"`
— owns the prompt completely, and disabling it lets the built-in floor
render again.

The floor itself has one knob, `NIU_PROMPT_CWD_STYLE` (`home` by default;
also `full` or `basename`), set in `~/.niubashrc`:

```bash
NIU_PROMPT_CWD_STYLE=full
```

Themes live in the external ecosystem (see the next section) — enable one
and its `PS1` takes over the prompt; disable it and the floor comes back.

## 7. Plugins: the external ecosystem

Themes and plugins come from external plugin managers, installed and
enabled through `niu plugin`. The curated catalog has `oh-my-bash`,
`bash-it`, and `bash-completion`; any git URL, `owner/repo` shorthand, or
local path works too. New sources are **untrusted by default** — nothing
they ship runs until you review and trust them:

```sh
niu plugin add oh-my-bash     # install from the catalog (untrusted)
niu plugin trust oh-my-bash   # review, then pass the execution gate
niu plugin enable oh-my-bash  # writes a guarded loader block into ~/.niubashrc
niu plugin enable agnoster    # a theme asset -> sets OSH_THEME in that block
niu plugin disable oh-my-bash # removes the block; the built-in floor returns
```

`niu plugin enable/disable` maintains one managed block per source between
`# >>> niu source <id>` / `# <<< niu source <id>` markers at the end of
`~/.niubashrc`; everything outside the markers is yours and is never
rewritten. Use `niu plugin list` and `niu plugin discover` for the current
inventory and state.

Executable CLI tools (starship, fzf, eza, bat, fd, ripgrep, zoxide, dust,
duf, erdtree, direnv, niu-git …) install through your real package
managers — niu downloads nothing (download retraction, owner ruling
2026-10-04). `niu plugin add fzf` prints the commands:

```sh
niu plugin add fzf        # prints: wpm install fzf (Windows, first choice),
                          #        winget/scoop alternatives, apt/dnf/brew,
                          #        plus the upstream release URL
```

Two systems coexist: the Unix command layer (`ls`, `cat`, `grep`, … plus
the application tools above) is owned by wpm on Windows (`wpm install
<tool>`, `wpm links rebuild`, `wpm update winuxcmd`) — wpm is the
first-class tool channel there (owner correction 2026-10-03); on Linux and
macOS the native package managers (apt/dnf/yum/brew) own that plane.
`niu plugin` owns the bash-ecosystem extension plane (git sources, themes,
completions) on every platform.

## What next

- [Plugin System Direction](../planning/plugin-system-direction.md) for the v3 plugin model
- [Plugin System Roadmap](../planning/plugin-system-roadmap.md) for the execution sequence
- [Oh My Niubash Ecosystem](../planning/oh-my-niu-ecosystem.md) for the external plugin ecosystem design
- [Roadmap](niubash-roadmap.md) to see what is planned
- Source at [github.com/unixwin/niubash](https://github.com/unixwin/niubash)
