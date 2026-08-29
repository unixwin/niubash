---
tags: [niubash, plugins, oh-my-winuxsh, bundle, roadmap]
created: 2026-07-30
status: draft
---

# Oh My Niubash Bundle Plan

`oh-my-winuxsh` is the official Niubash plugin bundle.

The synchronized execution plan is in
[Plugin System Roadmap](plugin-system-roadmap.md).

The goal is to make first-party Niubash plugins feel bundled, versioned,
updatable, and reversible, while keeping the core shell architecture stable:
rubash owns shell semantics, reedline owns interactive editing, and winuxcmd is
integrated through PATH.

## Target State

```text
niubash release
  niu.exe
  winuxcmd/
  bundles/
    oh-my-winuxsh/
      bundle.toml
      packs/
```

Users should be able to install a release zip and immediately have a local
baseline bundle. Network access is only needed for later bundle updates.

## Repository Reset Policy

The existing `unixwin/oh-my-winuxsh` repository is legacy content. Do not
delete repository history. Instead:

1. clone the repository;
2. tag the current state as `legacy-pre-niubash-plugin-system`;
3. create a `legacy` branch from the old state;
4. rebuild `main` around the new official bundle layout;
5. document the old repository content as legacy.

This preserves trust for open-source users while making the product direction
clean.

## Bundle Layout

```text
oh-my-winuxsh/
  README.md
  bundle.toml
  index.toml
  packs/
    git/plugin.toml
    docker/plugin.toml
    kubectl/plugin.toml
    npm/plugin.toml
    zoxide/plugin.toml
    direnv/plugin.toml
    dotenv/plugin.toml
    fzf/plugin.toml
    command-not-found/plugin.toml
    last-working-dir/plugin.toml
    thefuck/plugin.toml
    process-echo/plugin.toml
    keybindings/plugin.toml
    prompts/plugin.toml
  aliases/
    git.toml
    docker.toml
    kubectl.toml
    npm.toml
  completions/
    git.toml
    docker.toml
    kubectl.toml
    npm.toml
  prompts/
    segments.toml
  keybindings/
    common.toml
    emacs.toml
    vi.toml
  docs/
    design.md
    migration.md
```

## Bundle Metadata

```toml
name = "oh-my-winuxsh"
version = "1.0.0"
publisher = "unixwin"
api = "niubash:plugin-bundle@0.1.0"
min_niubash = "0.8.3"
channel = "stable"

[update]
source = "github-release"
repo = "unixwin/oh-my-winuxsh"
asset = "oh-my-winuxsh-{version}.zip"

[packs]
default = ["git", "prompts", "keybindings"]
available = [
  "git",
  "docker",
  "kubectl",
  "npm",
  "zoxide",
  "direnv",
  "dotenv",
  "fzf",
  "command-not-found",
  "last-working-dir",
  "thefuck",
  "prompts",
  "keybindings",
]
```

## Pack Categories

| Category | Packs | Default |
| --- | --- | --- |
| Daily dev | `git`, `docker`, `kubectl`, `npm` | `git` only |
| Navigation | `zoxide`, `fzf`, `last-working-dir` | Off |
| Environment | `direnv`, `dotenv` | Off |
| UX presets | `prompts`, `keybindings` | On if safe |
| Hints/workflow | `command-not-found`, `thefuck` | Off |

Packs that read project files, mutate environment, change cwd, or execute
external commands must stay explicit opt-in.

## Local Install State

```text
%LOCALAPPDATA%/Niubash/bundles/oh-my-winuxsh/<version>/
%LOCALAPPDATA%/Niubash/bundles/oh-my-winuxsh/current
<niubash install dir>/bundles/oh-my-winuxsh
~/.niubash/plugin-lock.toml
```

The app-bundled path is the offline baseline that ships with Niubash installers
and portable zips. User-managed bundle installs and the lock file take priority
so `niu plugin update oh-my-winuxsh ...` can move independently of app
updates.

`plugin-lock.toml` should record:

- bundle name;
- version;
- source URL or release ID;
- checksum;
- installed path;
- active path;
- previous version for rollback.

Release publication should also include `CHANGELOG.md` and
`docs/compatibility.md` from the bundle repository so API, protocol, minimum
host, and semver policy are visible next to each zip.

## CLI Surface

Initial commands:

```sh
niu plugin list
niu plugin info git
niu plugin search devtools
niu plugin themes
niu plugin plan enable git
niu plugin install git
niu plugin enable git
niu plugin disable zoxide
niu plugin update oh-my-winuxsh --from dist\oh-my-winuxsh-1.0.0.zip --checksum-file dist\oh-my-winuxsh-1.0.0.zip.sha256
niu plugin update oh-my-winuxsh --github-release latest
niu plugin update oh-my-winuxsh --github-release v1.0.0 --json
niu plugin rollback oh-my-winuxsh
```

The plan/apply behavior should mirror safe managed-config updates:
preview first, write only managed TOML blocks, create backups, and keep rollback
instructions explicit.

## Config Boundary

The manifest-backed registry records managed plugin CLI state:

```toml
[plugins]
enabled = true
bundles = ["oh-my-winuxsh"]
load = ["git", "prompts", "keybindings"]

[plugins.git]
enabled = true
permissions = ["cwd:read", "process:run:git"]
```

Normal user-authored plugin/theme selection and shell code belongs in
`~/.niubashrc`:

```sh
alias ll='ls -la'
export EDITOR=vim
```

The legacy TOML rc path is removed. RC is the human entry point, while the
plugin system still needs deterministic, auditable, machine-editable manifest
state for CLI-managed permissions, bundle versions, tests, and rollback.

## Migration From Current Niubash

1. Keep current behavior working.
2. Add plugin registry entries for existing built-in packs.
3. Add `[plugins]` config as the managed machine-editable surface.
4. Update docs and CLI help to say "official Niubash plugins".
5. Add `oh-my-winuxsh` bundle update support.

## Repository Status

The remote repository has been cloned locally at:

```text
C:\Users\caomengxuan\repo\oh-my-winuxsh
```

Current branch:

```text
codex/rebuild-official-plugin-bundle
```

Legacy state has been preserved locally as `legacy-pre-niubash-plugin-system`.
The active branch is being rebuilt as the official Niubash plugin bundle.
