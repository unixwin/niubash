# Prompt ownership and the external plugin ecosystem

Reference for working on `~/.niubashrc`, the plugin manifest, and the
generated loader. User-facing entry points: `niu setup` (interactive
wizard), `niu plugin` verbs, `niu font` (Nerd Font install), `niu doctor`
(verify). This document is for when an AI needs to write or repair these
files directly.

## The rc contract

`~/.niubashrc` is **plain bash** — aliases, exports, functions, whatever
claims `PS1`. The built-in plugin/theme stack is retired (niubash#145):
`NIU_THEME`, `NIU_THEME_PLUGIN`, `NIU_PLUGINS`, `NIU_BANNER`, `NIUBASH`
and `oh-my-niu.niu` lines in an old rc are legal assignments nothing reads
any more — they are inert, not errors.

```sh
# ~/.niubashrc — the shape `niu setup` generates
NIU_PROMPT_CWD_STYLE='home'      # floor knob: cwd shape in the default prompt
NIU_COMPLETION_STYLE='column'    # floor knob: completion menu (export in ~/.niubashrc)

# ... HOME normalization bootstrap (wizard-managed), then your own lines:

alias ll='ls -la'
export EDITOR='vim'
```

Rules:

- The built-in prompt is a floor, not an identity: it renders only while
  nothing claims `PS1`. A hand-set `PS1`, an enabled oh-my-bash theme, or
  `eval "$(starship init bash)"` owns the prompt completely; disabling
  it lets the floor render again.
- `NIU_THEME` does nothing. Themes come from external frameworks through
  the plugin system (below) — don't resurrect the retired variables.
- No plugin verb ever writes `~/.niubashrc`. If a tool claims it needs
  to, it is outdated — the manifest + generated loader carry activation.

## The plugin plane — manifest, lockfile, generated loader

Three files, three owners:

| File | Role | Written by |
|---|---|---|
| `~/.niubash/plugins.json` | declarative spec — what should be installed and enabled | `niu plugin` verbs, or hand-edit + `niu plugin sync` |
| `~/.niubash/sources/registry.json` | lockfile — what is installed (commit + tree checksum pins) | `niu plugin` verbs |
| `~/.niubash/generated/plugins.niu` | derived activation plane — one section per enabled source | `niu plugin` verbs, validate-then-atomic publish |

The loader is **host-owned and read-only**: never edit it — the next verb
regenerates it. At startup the host sources it BEFORE `~/.niubashrc`, so
user overrides win. Two conditions keep it off:

- The rc still carries a superseded managed block (`# >>> niu source ...`
  markers) — the double-execution guard prefers the live block; a one-time
  hint names `niu plugin migrate-rc`, the read-only review (`--diff`
  writes a suggested rc under `~/.niubash/candidates/`; the removal is
  always the user's own edit).
- `NIU_PLUGIN_LOADER=off` (also `false`/`0`/`no`) — the rollback switch.
  It stops loading only; unsetting rolls forward.

Spec shape:

```toml
schema = "niubash:plugin-spec@0.1.0"

[[sources]]
target = "oh-my-bash"            # catalog id | owner/repo | url | path
enable = ["git", "npm"]          # manager-native asset names
theme  = "agnoster"              # theme pick → OSH_THEME in the loader
```

## External sources — oh-my-bash, bash-it, and friends

```bash
niu plugin discover            # read-only overview of the ecosystem
niu plugin add oh-my-bash      # declare in the spec + git clone (untrusted)
niu plugin trust oh-my-bash    # review, then pass the execution gate
niu plugin enable agnoster     # theme pick → OSH_THEME in the loader
niu plugin enable git          # enable one asset (plugin/alias/completion)
niu plugin disable oh-my-bash  # deactivate; the floor prompt returns
niu plugin sync                # reconcile spec ↔ lockfile ↔ loader
niu plugin list                # sources, assets, activation state
niu plugin ui                  # menu over the same verbs
```

- Sources are git clones under `~/.niubash/sources/` (`NIU_PLUGIN_SOURCES_ROOT`
  relocates). Trust is explicit — an untrusted source contributes nothing.
- Theme names are the framework's own (`OSH_THEME` for oh-my-bash,
  `BASH_IT_THEME` for bash-it); `niu plugin list` / `discover` name the
  theme assets a source ships. Don't invent names.
- Icon themes render placeholders without a Nerd Font — `niu font` first.
- `starship` needs no source: `eval "$(starship init bash)"` in the rc.
- China-network git fetches can ride a mirror: `niu plugin mirror set <url>`
  (git `insteadOf` rewrite; recorded origins stay canonical).

## Validation flow

```bash
niu plugin list                # is the source declared + trusted + enabled?
cat ~/.niubash/generated/plugins.niu   # did the loader pick it up?
niu doctor                     # full install health (links, rc, loader, font)
printf '%s\n' "$PS1"           # who owns the prompt right now
```

Symptoms → causes:

- Plugin silent after `enable` → source not trusted yet (`plugin trust`),
  the spec was hand-edited without a `plugin sync` (verbs republish the
  loader themselves), or `NIU_PLUGIN_LOADER=off` is set — and the change
  needs a new shell or `source ~/.niubashrc` to take effect.
- Old `# >>> niu source` markers still in the rc → the loader stays off
  by design; `niu plugin migrate-rc --diff` shows the post-migration rc.
- Boxes/garbled glyphs → Nerd Font missing (`niu font`) or the terminal
  font isn't set.
- `NIU_THEME=...` in the rc does nothing → retired stack (niubash#145);
  pick the theme through `niu plugin enable <name>` or a hand-set `PS1`.
