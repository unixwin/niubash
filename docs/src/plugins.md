# Plugins & the Bash Ecosystem

Niubash does not vendor a plugin framework. The plugin system is the **external
bash ecosystem as first-class content**: oh-my-bash, bash-it and
bash-completion install over `git clone` and expose their real
themes/plugins/aliases/completions through
their own selection mechanisms. Nothing is sourced until you enable it.

This page is the user guide; the last section is a compact quick reference for
AI agents driving niu on a user's behalf.

## The five nouns

| Noun | What it is | Where it lives |
|---|---|---|
| **Source** | A git tree installed by niu (a manager like oh-my-bash, or any single-purpose repo) | `~/.niubash/sources/<id>/` |
| **Asset** | A theme/plugin/alias/completion *inside* a manager source | enabled via the manager's own mechanism |
| **Recipe** | A data row describing one ecosystem asset and how to install it (~500-row compiled-in index) | `niu plugin recipe list` |
| **Collection** | A named manifest of recipe ids — a starting-point bundle (LazyVim-extras pattern) | built-in, or `~/.niubash/distros/` |

One install driver exists, and only one: `git` (tree sources, via
`niu plugin add <git-url>`). Niubash carries zero network/HTTP download
responsibility (download retraction, owner ruling 2026-10-04): executable
tools (fzf, starship, …) install through your real package managers —
wpm first on Windows (owner correction 2026-10-03), winget/scoop as
alternatives for what wpm does not carry (GUI apps, fonts), and
apt/dnf/yum/brew on other platforms. The recipe rows for those tools stay
in the index as catalog metadata and `add` prints the commands.

## Everyday flow

```console
$ niu plugin discover              # read-only: what's out there
$ niu plugin add oh-my-bash        # git-clones the manager
$ niu plugin list                  # sources, assets, activation state
$ niu plugin enable agnoster      # activate a theme through oh-my-bash
```

The load-bearing rule: **install and activation are separate
verbs**. `add`/`apply` land content on disk but nothing from it executes in
your shell until you `enable` an asset explicitly. Collection applies honor
the same rule. (There is no trust/review gate — niubash#647 removed it;
what you install is what runs once enabled, exactly like cloning a repo
and sourcing it by hand.)

Manager sources (catalog ids like `oh-my-bash`) are also declared in the
spec `~/.niubash/plugins.json` — the single source of truth for what is
declared; `niu plugin sync` reconciles the spec with what is installed,
and `niu plugin update <id>` (no id = all) moves sources to their ref tips.

What activation writes is never your rc (niubash#359, v1.4.2): every mutating
verb regenerates `~/.niubash/generated/plugins.niu` — a deterministic,
validate-then-publish loader the host sources read-only before
`~/.niubashrc`, so user rc lines win. `NIU_PLUGIN_LOADER=off` disables
loading; legacy `# >>> niu source` managed blocks left in an old rc keep the
loader off until you remove them (`niu plugin migrate-rc --diff` diagnoses —
it never edits the rc either).

### Imperative mode → adoption (1.3.1)

A machine with installed sources but no spec is in *imperative mode*: the
startup form stays silent (nothing to reconcile), and
`niu plugin sync` lists the state with the migration one-liner:

```console
$ niu plugin sync --adopt
declared oh-my-bash (installed; adopted into the spec)
...
```

`--adopt` declares every installed-but-undeclared source, snapshotting the
live enablement (`enable = [...]`) and the active theme (`theme = ...`)
from the machine's current state, so the adopted spec round-trips — a
plain `niu plugin sync` afterwards is a no-op. `niu plugin add <target>`
on an already-installed source declares it (never the old "remove it
first" refusal), and the first-run wizard's collection apply now ends
spec-managed (the run itself adopts, picked theme included). Startup
installs that fail are memoized and not retried on every terminal —
explicit verbs retry.

Undo is printed with every mutating command (`disable`, `source remove`,
`source rollback`) and the setup wizard journals one undo
command per thing it changed.

## Recipes: the index

```console
$ niu plugin recipe list                     # the whole index
$ niu plugin recipe list --category prompt   # manager|theme|plugin|alias|completion|prompt
$ niu plugin recipe list --json              # machine-readable (agents: prefer this)
$ niu plugin recipe show starship            # driver, version, license, state
$ niu plugin add starship                    # prints package-manager install commands
```

Rows with `driver = git` install as tree sources; executable-tool rows
(`driver = "download"` in the generated index) are catalog metadata —
`add` prints package-manager install commands (wpm first on Windows,
native managers elsewhere) and never fetches; rows with no driver are
info-only (e.g. ble.sh, which builds from source) and `add` explains
instead of installing.

## Collections: starting points

```console
$ niu plugin distro list            # built-in + imported
$ niu plugin distro apply recommended
$ niu plugin distro import <git-repo-or-path>   # any repo with niu-collection.json
$ niu plugin distro remove <name>
```

Built-ins: `minimal` (bash-completion only), `recommended` (oh-my-bash + its
default theme + completions — also offered by the first-run wizard),
`full` (both frameworks + bash-preexec; its fzf/starship entries print
package-manager suggestions, since niu downloads nothing).

A collection is a small JSON manifest of recipe ids:

```json
{
  "schema": "niubash:plugin-collection@1",
  "name": "my-starter",
  "description": "what this bundle is",
  "entry": [
    { "recipe": "oh-my-bash" },
    { "recipe": "starship" }
  ]
}
```

Imports are first-class: anything the built-ins can do, your own repo can do,
byte-identically. Upgrading from a pre-1.5.0 niu whose imported collections
still use the TOML forms? Run `niu plugin migrate` once — it converts the
distro registry and every imported manifest to JSON and renames the old
files aside; nothing reads the TOML forms afterwards. The same one-shot
bridge also converts the plugin spec (`~/.niubash/plugins.toml` →
`plugins.json`, merged entry-by-entry by target) and the source registry
(`~/.niubash/sources/registry.toml` → `registry.json`, merged by source
id) — existing JSON entries win, legacy-only entries are carried over,
and the old files are renamed aside, never deleted. `apply` installs every entry through its recipe driver —
installation only; activation stays an explicit `enable`. Failures are collected per
entry (a bad entry never kills the rest) and reported with the retry verb.
The first-run wizard's plugin-collection question is a LazyVim-style
progressive-disclosure hook: it only appears when the ecosystem is empty, and
Skip is the default.

When the picked collection installs a theme-bearing source (`recommended`/
`full`), the same wizard run immediately offers the theme pick — one run,
out of the box. The gallery lists the fresh
source's themes, and the pick lands through the same manifest +
generated-loader path a later `niu plugin enable <theme>` uses, journaled
with its own undo line. Declining (or Esc) changes nothing — the run prints
the re-run/enable follow-up instead.

## Tools: package-manager recommendations

```console
$ niu plugin add fzf          # prints: wpm install fzf (Windows, first choice),
                              #        winget/scoop alternatives, apt/dnf/brew,
                              #        plus the upstream release URL
```

Nothing is fetched and nothing lands on disk — run the printed command with
your package manager. The retired `niu plugin tool` verbs fail with a
pointer here; a download-era install under `~/.niubash/tools/<id>` can be
removed by deleting the directory.

## The menu UI

`niu plugin ui` opens a menu-level UI (needs an interactive terminal):
sections ordered by what needs attention — sources whose tree is missing
first, then ready sources, installable recipes, collections, global verbs. Every
action in the menu is the same verb the CLI runs (the footer shows the CLI
equivalent of each); there is no UI-only logic. Rows read
`[state] id — hint`.

## For AI agents (quick reference)

When driving niu on a user's machine:

- Prefer `niu plugin recipe list --json` and `niu plugin list [--json]` for
  state; plain-text output is for humans and changes freely.
- **Install is not activation.** `niu plugin add ...` / `distro apply ...`
  land content on disk but nothing executes until an explicit
  `niu plugin enable ...`. There is no trust gate (niubash#647): once the
  user enables an asset, it runs as-is.
- A source whose tree is missing cannot activate — suggest
  `niu plugin restore <id>`.
- Mutations print their undo (`disable`, `source remove`,
  `source rollback <id>`). Surface those undo verbs to the user.
- `niu plugin update <id>` moves a source to its ref tip (no id = every
  source); `niu plugin sync` reconciles the spec with reality;
  `niu plugin restore` rebuilds from the lockfile pin in
  `~/.niubash/sources/registry.json`.
- Exit codes: success `0`; usage errors and refused operations (unknown id,
  builtin-name collision, non-tty `ui`) exit non-zero with a message that
  names the object and the repair verb.
- Env overrides used by tests/tools: `NIU_PLUGIN_SOURCES_ROOT`,
  `NIU_PLUGIN_DISTROS_ROOT`.
