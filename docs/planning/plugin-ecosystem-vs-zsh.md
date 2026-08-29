# Plugin ecosystem: niubash vs zsh — openness audit

Status: planning input for the post-1.0.0 ecosystem roadmap.
Scope: how open is the niubash/oh-my-niu plugin story compared with the zsh
ecosystem, and which gaps are worth closing first.

## Verdict

Feature coverage is competitive; **openness is the real gap**. The zsh world
lets anyone ship a plugin by pushing a git repo; niubash today funnels
everything through one officially curated bundle (oh-my-niu) and a
manifest-backed registry. That design bought us a permission model and
reviewed defaults, but it leaves three distribution channels missing.

## Where zsh is

| | oh-my-zsh | zinit / antidote | raw zsh conventions |
|---|---|---|---|
| Plugins | ~330 bundled, ~1000+ indexed (awesome-zsh-plugins) | any git repo | any dir with `*.plugin.zsh` |
| Install | framework-bundled | `zinit light user/repo` | `git clone` + one `source` line |
| Themes | ~150 bundled | any repo | one prompt function file |
| Trust | you source random code | you source random code | you source random code |
| Cost of authoring | one file, zero metadata | one file | one file |

The zsh ecosystem's superpower is a **zero-metadata convention**: a directory
with a `.plugin.zsh` file is a plugin. Discovery (awesome lists), loading
(zinit et al.) and safety (none) are all layered on top by other tools.

## Where niubash is

- One official bundle (oh-my-niu) with ~40 reviewed packs; manifest declares
  permissions; source packs may only load bundle-local scripts.
- Process plugins give an ABI for external adapters (debug bridges, providers).
- Local customization lives in `~/.niubash/custom` (manual file placement).
- External completion definitions already load from TOML, and the
  manifest-backed registry is the control plane.

## The openness gaps

1. **No third-party distribution channel.** There is no
   `niu plugin add <git-url>`; the only blessed bundle is oh-my-niu.
2. **Single-curator registry.** New packs must pass the official review
   queue; there is no federation of community indexes.
3. **Heavy authoring convention.** A zsh plugin is a file; an oh-my-niu pack
   is manifest + layout + review. Nothing accepts "just a directory".
4. **No theme long tail.** Themes exist but community contribution has no
   self-serve path.
5. **Discovery is zero.** No index page, no `niu plugin search`.

## What we should NOT copy

- Unreviewed code execution by default. The permission model is a feature;
  zsh's "source anything" is a supply-chain incident waiting to happen.
- A second package manager for binaries (WPM already owns that lane).

## Roadmap proposal

- **P0 — third-party install channel.** `niu plugin add <git-url>[@ref]`
  clones into `~/.niubash/external/<name>`, registers as `external_bundle`
  (untrusted), and requires an explicit `niu plugin trust <name>` before
  source packs activate. `niu plugin list/remove/search` round it out.
- **P1 — federated indexes.** `index.toml` gains sources: official by
  default, user-added git/URL indexes appended. Discovery without curation.
- **P2 — classic mode.** Accept a plain directory (or single `.winux` file)
  as a "classic" pack: auto-generated minimal manifest, alias/function-only
  scope, no process plugins without an explicit manifest.
- **P3 — oh-my-zsh bridge (explicitly a shim, not a goal).** Load pure
  alias/function oh-my-zsh plugins read-only; document that completion and
  hook-heavy plugins are out of scope.
- Governance invariant: review gates apply to the *default/official* channel
  only; third-party bundles are opt-in, untrusted until trusted, and every
  permission grant is user-visible.

## Sizing note

P0 is mostly host-side (plugins/mod.rs already has bundle install/update/
rollback machinery over local paths; adding a git-clone source and an
external registry entry is incremental). P1/P2 are registry format work.
P3 should stay tiny or die.
