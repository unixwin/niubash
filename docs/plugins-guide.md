# Niubash Plugins Guide

The plugin system treats the **external bash ecosystem as first-class
content**: any sourceable bash — a known plugin manager (oh-my-bash,
bash-it, bash-completion), a tree installed by bpkg, or an arbitrary wild
repo / single-file plugin — installs on explicit command, earns activation
only through a trust review, and loads through its **own** loader, never
through a niubash re-implementation of it.

The declarative spec `~/.niubash/plugins.toml` is the single source of
truth for *what should be installed and enabled*. The CLI verbs are sugar
over the spec; `niu plugin sync` reconciles the spec with the machine.

## The three layers

| Layer | What lives there | Example |
|---|---|---|
| Base (wild) | Any repo or file with sourceable `*.sh` / `*.bash` | `niu plugin add rcrowley/bash-preexec`, a gist-style `spark.bash` |
| Managers | oh-my-bash, bash-it, bash-completion, bpkg trees | `niu plugin add oh-my-bash` |
| Red lines | Engine has zero plugin special-cases; no shims | framework plugins fail *identically* to GNU bash when sourced bare |

Wild sources are enumerated **honestly**: every `*.sh`/`*.bash` file is
listed as a candidate with a tag (`script`, `fragment`,
`installer/test-like — review before sourcing`, `bpkg script`). Nothing is
hidden and niubash never guesses "the" entry — you pick files explicitly.
Enabling a file adds one guarded line to the managed rc block:

```sh
if [ -r "${NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}/<id>/<file>" ]; then
  . "${NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}/<id>/<file>"
fi
```

This is byte-faithful to manually sourcing the file under GNU bash —
including the errors a plugin produces when its framework is missing
(§14.4, no shims; see *Troubleshooting*).

## The spec: `~/.niubash/plugins.toml`

```toml
schema = "niubash:plugin-spec@0.1.0"

[[sources]]
target = "oh-my-bash"            # catalog id | owner/repo | git url | local path
enable = ["git", "npm"]          # manager-native selection (OMB: plugins=() rc arrays)
theme  = "agnoster"              # optional: OSH_THEME / BASH_IT_THEME; absent = unmanaged

[[sources]]
target = "rcrowley/bash-preexec" # wild file source: GitHub shorthand
id     = "bash-preexec"          # bound automatically at first sync
enable = ["bash-preexec.sh"]     # asset names are tree-relative paths
```

Fields:

| Field | Meaning |
|---|---|
| `target` | The reproducible origin exactly as resolvable at sync time: catalog id, `owner/repo`, git url, or a local path. An explicit `--path`/`--url` at add time is stored as the target (a catalog id would otherwise re-resolve to the network URL). |
| `id` | Source id. Managers: the manager id (one install per machine). Wild/bpkg: derived from the origin tail at first sync and written back. |
| `kind` | Optional adapter pin (e.g. `bpkg` for an adopted tree): later syncs reinstall through that adapter and *refuse* if the tree stops matching its fingerprint. |
| `ref` | Git ref for the first fetch only; later syncs never move the lockfile pin — `niu plugin update` does. |
| `enable` | Enabled assets in the manager's own vocabulary: OMB rc arrays, bash-it `enabled/` entries, per-file source lines for wild/bpkg. |
| `theme` | Theme pick. Absent means "unmanaged": a hand-set theme variable survives syncs. `theme = ''` explicitly clears it. |

**Do not hand-write the theme variable in the rc** (niubash#196).
`export OSH_THEME=...` typed directly into `~/.niubashrc` is not the
supported channel: sync materializes the managed theme block from this
`theme` field, and the hand-written line is dropped or overridden on the
next sync — the theme silently falls back. `niu plugin sync` detects a
stray hand-written theme assignment outside the managed blocks and prints
a one-time stderr warning pointing at the spec.

**Theme ownership is exclusive.** The same theme name can ship in several
frameworks (powerbash10k exists in oh-my-bash AND bash-it), so a theme pick
made through the setup wizard/gallery or `niu plugin enable <theme>` claims
the name for exactly one source — it writes the picked entry's `id` + `theme`
and moves the claim away from any other entry (a claimant left with no other
selection loses its declaration, and sync then drops its activation block).
If a spec still carries a stale claim (an older version wrote the rc without
updating the spec, or a hand edit), the next `niu plugin sync` reconciles it
toward the rc's live state — the working theme block never flips; sync clears
the stale claim and prints the `reconciled` row naming the owner. The floor
(§14.5: a declared source is active even with an empty selection) yields to a
claimed theme: a source with no selection of its own does not load its
framework on top of another source's claimed theme.

The spec declares *what should exist*; the registry
(`~/.niubash/sources/registry.toml`, schema `@0.3.0`) locks *what exists*
(commit + tree checksum pins) and remembers the last selection the spec
materialized (`spec_enabled`/`spec_theme`). This is the same spec + lock
pair lazy.nvim/vim-plug use.

## Three ways to add a plugin

| Command | Spec entry written | Notes |
|---|---|---|
| `niu plugin add oh-my-bash` | `target = "oh-my-bash"`, `id = "oh-my-bash"` | Catalog shorthand; installs from the official origin |
| `niu plugin add rcrowley/bash-preexec` | `target = "rcrowley/bash-preexec"` | Wild source; id derived (`bash-preexec`) and bound at first sync |
| `niu plugin add bpkg --path <dir>` | `target = "<dir>"`, `kind = "bpkg"` | Adopt a tree `bpkg install` already downloaded; per-package id; npm-shaped `package.json` is refused |

Every `add` lands the source **untrusted**: nothing activates until you
review and trust it. All three are one spec entry + one sync under the
hood — you can equally hand-edit the spec and run `niu plugin sync`.

## Collections: the out-of-box sets

A collection ("distro", the LazyVim extras pattern) is a **data manifest**
listing recipe ids; `niu plugin distro apply <name>` walks the entries
through the same add pipeline as `niu plugin add` — every source lands
**untrusted**, failures are reported per entry (a bad entry never kills
the rest), and the setup wizard offers the same sets on first run. Three
built-ins ship in the product (`niu plugin distro list`); the same
manifest format can be imported from any repo or directory
(`niu plugin distro import <url|dir>`).

Since niubash#171 the built-ins carry **independent recipes** too —
completion-script repos, plugin files and hook layers installed through
the plugin driver's own git install, not only framework themes. Every
curated entry file is audited against its upstream root (the seed test
fails on drift before a user ever sees it). Executable-tool rows are
**not** collection entries: fzf and starship stay package-manager
recommendations (download retraction — niu downloads nothing; `niu plugin
add fzf` prints the commands).

| Collection | Entries |
|---|---|
| `minimal` | bash-completion |
| `recommended` | oh-my-bash, omb-theme-robbyrussell, bash-completion, bash-preexec, complete-alias, fzf-git.sh |
| `full` | everything in `recommended` **plus** bash-it, bash-sensible, git-flow-completion |

The independent recipes in one line (upstream, license, entry file):

| Recipe | What it is | Upstream | License | Entry |
|---|---|---|---|---|
| `bash-preexec` | precmd/preexec hook layer | github.com/rcaloras/bash-preexec | MIT | `bash-preexec.sh` |
| `complete-alias` | alias-aware completion | github.com/cykerway/complete-alias | GPL-3.0-only | `complete_alias` |
| `fzf-git.sh` | fzf key bindings for git objects (script only; needs the fzf binary from your package manager) | github.com/junegunn/fzf-git.sh | MIT | `fzf-git.sh` |
| `bash-sensible` | sane bash defaults | github.com/mrzool/bash-sensible | MIT | `sensible.bash` |
| `git-flow-completion` | git-flow completion | github.com/bobthecow/git-flow-completion | MIT | `git-flow-completion.bash` |

## What `niu plugin sync` does

`niu plugin sync` reconciles spec and machine (lazy.nvim `:Lazy sync`
semantics):

- **declared, not installed** → fetch through the add pipeline. The fetch
  gate runs automatically; the **trust gate never does** — the source
  lands untrusted and sync prints the exact `niu plugin trust <id>`.
- **declared, installed, trusted** → the managed rc block / enabled tree
  is (re)materialized **from the spec, idempotently**: an unchanged spec
  produces a byte-identical rc.
- **installed, untrusted** → reported as `awaiting-trust`.
- **installed, not declared** → suggested for cleanup, never auto-deleted.
  `niu plugin sync --prune` is the explicit confirm.

`niu plugin sync --bootstrap` is the same reconciliation in its quiet
startup form — wired into `~/.niubashrc` by the setup wizard as a single
line, silent when everything is in sync. Disable it by setting
`NIU_PLUGIN_BOOTSTRAP=off`. `niu doctor` carries an advisory line showing
the spec reconciliation state (declared / missing / undeclared / no spec).

Two 1.3.1 guarantees for the startup form:

- **No spec → no startup nag.** Imperative mode (no `plugins.toml`) means
  *nothing to reconcile*: installed-but-undeclared sources are the legacy
  state, not drift, and `sync --bootstrap` stays silent about them. The
  interactive `niu plugin sync` still lists them — with the migration verb.
- **Failed startup installs are memoized.** When a declared source cannot
  install at startup (offline, a moved origin, a fingerprint the adapter
  rejects), the failure is recorded under the sources root
  (`bootstrap-failures.toml`) and later startups defer the retry with one
  line instead of re-fetching on every terminal. An explicit
  `niu plugin sync` / `niu plugin add` clears the memo and retries.

### Imperative mode → spec adoption (`niu plugin sync --adopt`)

Machines that installed sources before the spec existed (1.3.0's wizard
collection apply, `niu plugin source add`) land in *imperative mode*: the
registry has sources, `plugins.toml` does not exist. `niu plugin sync`
then reports the state and names the one-line migration:

```
niu plugin sync --adopt
```

`--adopt` declares **every installed-but-undeclared source** into the
spec, snapshotting the current live enablement into `enable = [...]` and
the active theme into `theme = ...` (targets are the recorded origins,
ids pinned). The adopted spec **round-trips**: the `--adopt` run itself
reconciles, and a plain `niu plugin sync` afterwards is a no-op —
byte-stable rc blocks, unchanged registry state. Existing entries are
never touched (a defensive merge), and `niu plugin add <target>` on a
source that is already installed declares it too (printed as `Declared …
(already installed)`) instead of refusing.

The setup wizard does the same adoption at the end of a collection apply
(after the post-install theme pick), so a fresh 1.3.1+ wizard run ends
**spec-managed**: sources, trust state and the picked theme are all
described by `~/.niubash/plugins.toml`, and no migration verb is needed.

### Merge semantics: spec vs your hand edits

`sync` computes `prev` (the selection the spec last materialized — the
spec-owned set only), reads the live block, and treats everything in the
live block that is not in `prev` as **hand-added**:

- the new materialized set is `next(spec) ∪ hand_added`;
- entries the spec dropped (`prev − next`) are removed;
- hand-added entries survive **every** sync — they are never absorbed
  into the spec's own set, so a later spec change cannot remove what you
  typed by hand (remove hand entries by editing the rc or via
  `niu plugin disable`);
- a theme: the spec's pick wins when declared; a spec that declares none
  keeps the current (manual) theme.

bash-it's `enabled/` directory is reconciled with the same rule.

## The trust flow

```text
niu plugin add <target>      # installs untrusted (fetch gate)
niu plugin trust <id>        # review output, then activate the source
                             # (hash-lock tier: tree checksum pinned)
niu plugin enable <id>[/<asset>]   # declare + materialize
```

- **Trust review** (`niu plugin trust`) shows origin, version, license and
  the tree checksum before flipping the gate.
- The default tier is a checksum lock. `niu plugin source sign <id>`
  pins the tree with a local signature; updates then re-gate until
  re-signed (`niu plugin source verify <id>` checks either tier).
- Untrusted sources never contribute assets: `niu plugin list` hides them
  and `enable` refuses.
- Lockfile verbs: `update` moves the pin to the ref's tip, `restore`
  rebuilds the tree from the pin, `rollback` returns to the previous
  state, `clean` removes staging leftovers and orphaned trees.
- **The setup wizard's post-install pick** (one run, out of the box): when
  a collection applied by `niu setup` installs a theme-bearing source
  (e.g. the `recommended` collection's oh-my-bash), the same run asks one
  trust question — the wizard's phrasing of `niu plugin trust`, same
  checksum tier — and then offers the freshly trusted source's themes;
  the picked theme lands through the same guarded block `niu plugin
  enable <theme>` writes and gets its own undo line in the setup journal.
  Declining changes nothing: the run prints the exact `niu plugin trust
  <id>` command (plus the re-run / enable follow-up) to do it later.

## Troubleshooting

- **`_omb_module_require: command not found` when sourcing a plugin file
  directly** — that is *correct* fidelity, not a bug: a framework plugin
  (e.g. bashmarks) sourced without its manager produces exactly the error
  GNU bash produces. Enable it through the manager
  (`niu plugin enable oh-my-bash/bashmarks`) so the framework's own loader
  provides the dependency. niubash deliberately ships no shim (§14.4).
- **`source ... does not look like 'bpkg'`** — the adopted tree's manifest
  does not match the bpkg fingerprint (a `bpkg.json`/`package.json` whose
  `scripts` is an *array* of file paths). npm's object-shaped `scripts`
  is rejected by design; adopt such trees as plain file sources instead
  (drop the `bpkg` kind).
- **`no supported plugin manager detected ... no sourceable *.sh/*.bash
  files either`** — the repo has nothing niubash could honestly source.
- **`installed but not declared in the spec`** — an imperative install
  (`niu plugin source add`) with no spec entry; run `niu plugin sync
  --adopt` to declare everything installed (snapshots the live state), or
  prune it explicitly. With no spec at all this is imperative mode and the
  startup form stays silent about it.
- **`deferred` rows at startup** — a declared source's install failed at a
  previous startup and is not retried there; run `niu plugin sync` (the
  explicit verb) to retry and see the failure.
- **`awaiting-trust` rows after sync** — sync never flips the trust gate;
  run the printed `niu plugin trust <id>` after reviewing.
- **`tree missing — repair with niu plugin restore <id>`** — the guarded
  loader no-ops silently at startup (the native fallback chain stays
  intact); restore rebuilds from the lockfile pin.
- **wild source enable says `pick one`** — file sources never guess an
  entry; `niu plugin list` shows every candidate with its tag, enable
  with `niu plugin enable <id>/<relative/path>.sh`.

## Executable tools and fonts install through package managers

Download retraction (owner ruling 2026-10-04): **niubash carries zero
network/HTTP download responsibility.** The plugin driver
(`niu plugin add <git-url>`, git clone only) is the *only* extension
installation entry. Executable tools (fzf, starship, ripgrep, bat, …) and
fonts are installed by your real package managers — the recipe rows stay
in the index as catalog metadata, and `niu plugin add fzf` prints the
commands instead of fetching anything:

- **Windows — wpm first** (owner correction 2026-10-03: wpm is the
  first-class command-layer tool installer; the niu shell stopped
  downloading, wpm was not demoted): `wpm install fzf`, with
  winget/scoop as alternatives for what wpm does not carry (fonts, GUI
  apps): `winget install --id junegunn.fzf`, `scoop install fzf`.
- **Linux/macOS — native package managers only**: `sudo apt install fzf`,
  `sudo dnf install fzf`, `brew install fzf`. No wpm strings exist at all
  in non-Windows builds.
- Every recommendation also prints the upstream release URL, so no row
  can dead-end. `niu font` likewise only detects installed Nerd Fonts and
  prints the winget/scoop/brew/nerdfonts.com commands.

Two systems coexist by design: **wpm = the Unix command layer on
Windows** (application tools included, wpm-first), **`niu plugin` = the
bash-ecosystem extension driver** (git sources, themes, completions, any
platform). The retraction story is "niu stopped downloading", not "wpm
was replaced by winget".

The retired `niu plugin tool list` / `tool remove` verbs fail with a
pointer to the package-manager flow; a download-era install under
`~/.niubash/tools/<id>` can be removed by deleting the directory.

## Git fetches stall? Configure a mirror

Every plugin git clone/fetch goes to GitHub. If that is slow or
unreachable from your network (common in mainland China), point niubash
at a mirror URL you trust — one command:

```sh
niu plugin mirror set https://your-mirror.example.com/github.com
# back to direct connection:
niu plugin mirror set none
```

The mirror is a **transport concern only**: the spec and the lockfile
keep canonical GitHub URLs, so a tree cloned through a mirror is
byte-identical to a direct one (checksum verification is unaffected — a
mirror is never a trust signal). Only `https://github.com/` origins are
rewritten, through git's own `insteadOf` mechanism; anything else passes
through untouched.

Mirrors are **git-only** since the retraction (the shell has no HTTP
transport left). The config lives in `~/.niubash/mirrors.toml` (see
`niu plugin mirror --help`):

```toml
schema = "niubash:mirrors@0.1.0"
active = "custom"

[github]
git_instead_of = "https://your-git-mirror/"    # git clone/fetch
```

niubash ships **no bundled mirror list**: community mirror services are
unsupported and may disappear at any time — the examples written as
comments in the config file are starting points, not endorsements.

## Environment overrides

| Variable | Effect |
|---|---|
| `NIU_PLUGIN_SPEC` | Alternate spec file location |
| `NIU_PLUGIN_SOURCES_ROOT` | Alternate install root (default `~/.niubash/sources`) |
| `NIU_PLUGIN_BOOTSTRAP` | `off` disables the rc bootstrap line |
| `NIU_MIRRORS` | Alternate mirror config (default `~/.niubash/mirrors.toml`) |

Design references: `docs/planning/oh-my-niu-ecosystem.md` §14.6 (three
layers, descriptor table, declarative spec), §14.4 (no shims), §14.8
(manager bundles with the product; download/git mirroring). A compact
machine-readable companion for agents lives in
`docs/plugins-quickref.md`.
