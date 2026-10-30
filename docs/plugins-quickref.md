# Niubash Plugin System — AI Quick Reference

Dense reference for agents operating niubash's plugin surface. Human
narrative: `docs/plugins-guide.md`. Design: `docs/planning/
oh-my-niu-ecosystem.md` §14.6. All verbs: `niu plugin --help`.

## Model

- **Spec = source of truth**: `~/.niubash/plugins.json` declares what
  should be installed/enabled. Registry (`~/.niubash/sources/registry.json`,
  schema `niubash:plugin-source-registry@0.3.0`) pins what exists
  (commit sha + tree sha256) and the last spec-materialized selection
  (`spec_enabled`, `spec_theme`).
- **CLI = sugar**: every mutating verb edits the spec then runs the
  reconciler (`plugins::sync`). Hand-editing the spec is equally valid.
- **Managers are data**: one descriptor table (`ManagerDescriptor` in
  `crates/niubash-runtime/src/plugins/descriptors.rs`) — id, origin,
  fingerprint, asset patterns, loader spec, selection spec. Adding a
  manager = one row, zero interpreter code. Current rows: `oh-my-bash`,
  `bash-it`, `bash-completion`, `bpkg`. Wild fallback adapter id `file`.
- **Id scope**: managers = one install per machine (source id = manager
  id); `file` sources and `bpkg` adoptions = one install per package
  (id = explicit `--id` or sanitized origin tail; bound into the spec at
  first sync).
- **Layout fingerprints** (detection, not filename blacklists): OMB =
  `oh-my-bash.sh`; bash-it = `bash_it.sh` + `lib/composure.bash`;
  bash-completion = `bash_completion`; bpkg = `bpkg.json`/`package.json`
  with `scripts` as an **array** (npm's object shape never matches).

## Verb table

| Verb | Effect | Exit |
|---|---|---|
| `add <id\|owner/repo\|url\|path> [--id N] [--ref R] [--checksum S] [--path D]` | Append spec entry + sync (install only; `--checksum` is an install-time pin checked against the fetch). A recipe id for an executable tool (fzf, starship, …) prints a package-manager recommendation instead — niu downloads nothing (retraction 2026-10-04) | non-zero if the entry failed to install or is already declared; recipe recommendations exit 0 |
| `list [--json]` | Sources, assets (`*` = enabled), tags for wild candidates | 0 |
| `enable <id\|asset\|id/asset>` | Spec edit + sync; wild/bpkg assets are tree-relative paths | non-zero when the tree is missing/ambiguous/whole-source misuse |
| `disable <target>` | Spec edit (entry removal for source targets) + sync; tree kept | 0 |
| `sync [--prune] [--adopt] [--bootstrap]` | Reconcile; `--prune` deletes undeclared, `--adopt` declares installed-but-undeclared sources into the spec (snapshots live enable/theme), `--bootstrap` accepted as a silent no-op for legacy rc lines (startup never syncs) | 0 |
| `update [<id>]` | Move lockfile pin(s) to ref tip (no id = all) | 0 |
| `restore [<id>]` / `rollback <id>` / `clean` | Lockfile verbs | 0 |
| `discover` | Read-only overview | 0 |
| `source <sub>` | Full source protocol (add/remove/update/rollback/list) | — |
| `mirror <sub>` | Git-only mirroring: `list`, `set <url\|none>` (insteadOf rewrite of git fetches) | non-zero on garbage URL |
| `recipe <sub>` / `distro <sub>` | Recipe index (list/show/add) / collections (list/import/remove/apply) | — |
| `tool <sub>` | **Retired** (download retraction): fails with a pointer to the package-manager flow | non-zero |
| `ui` | Menu over the same verbs | 0 |

## Sync row actions

`activated`, `unchanged`, `deactivated` (spec no longer declares; block
dropped, tree kept), `missing` (tree missing → `niu plugin restore`),
`failed`, `removed` (--prune), `unsupported`.

## Invariants (pinned by tests/plugin_spec_sync.rs, tests/plugin_anyplug.rs)

1. **Idempotent materialization**: unchanged spec → byte-identical managed
   rc block, all rows `unchanged`, report clean.
2. **Hand-added survival**: `prev` records only the spec-owned selection;
   live-block entries outside `prev` survive every sync; spec-dropped
   entries (`prev − next`) are removed. Theme absent in spec = keep
   current (manual) theme; `theme = ''` clears it.
3. **CLI-spec consistency**: `add` + `enable` produce the same spec file
   and rc block as hand-writing the spec + sync (byte-for-byte).
4. **Cleanup honesty**: undeclared sources are suggested, never
   auto-deleted; `--prune` is the explicit confirm (removes tree +
   registry entry).
5. **Install is not activation**: sync installs and materializes the
   declared selection; assets run only where the spec/rc selection
   enables them (no trust gate exists — niubash#647).
6. **Honest enumeration**: wild sources list every `*.sh`/`*.bash`
   (≤256, depth-first) with tags (`script`, `fragment`,
   `installer/test-like — review before sourcing`, `bpkg script`); no
   guessed entry; repos with nothing sourceable fail at add.
7. **No shims (§14.4)**: enabling a file adds a guarded `. <path>` line
   identical to manual sourcing; framework plugins sourced bare fail with
   the same diagnostic and exit status as GNU bash (pinned against a real
   bash when present in tests/plugin_anyplug.rs).
8. **Engine zero plugin special-cases**: rubash source contains no plugin
   dispatch (comments citing ecosystem scripts as regression provenance
   only; audited read-only 2026-10-02).
9. **Zero download responsibility (retraction 2026-10-04)**: niu has no
   HTTP/download transport at all. `niu plugin add <git-url>` (git clone)
   is the only extension installation entry; executable-tool recipes are
   catalog rows whose `add` prints package-manager recommendations —
   **wpm first on Windows** (owner correction 2026-10-03; wpm strings are
   `cfg(windows)`-gated and never compile elsewhere), native managers
   (apt/dnf/yum/brew) on other platforms, upstream URL always printed.
   Fonts are detection+recommendation only (`niu font`).
10. **Mirrors are transport-only (§14.8), git-only**: `~/.niubash/mirrors.json`
   (`NIU_MIRRORS`) injects `git -c url.<base>.insteadOf` for
   clone/fetch; spec/registry records keep canonical URLs;
   missing/malformed/unknown config degrades to direct; legacy
   `[github] prefix` / `[github.releases]` channels parse compatibly and
   are ignored (no HTTP transport exists to use them); no auto-select,
   no probing; zero bundled mirror list. Pinned by plugins::mirrors unit
   tests, sources.rs
   `git_clone_args_carry_instead_of_mirror_for_github_origins_only`,
   tests/plugin_mirrors.rs.

## Spec schema (`niubash:plugin-spec@0.1.0`)

```json
{
  "sources": [
    { "target": "oh-my-bash", "id": "...", "kind": "bpkg", "ref": "v1.2", "enable": ["git"], "theme": "agnoster" }
  ]
}
```

Field notes: `target` is the reproducible origin (catalog id | owner/repo |
url | path); `id` is optional (wild/bpkg ids bind at first sync); `kind` is
an optional adapter pin (the fingerprint must still match); `ref` is
optional (first fetch only); `enable` lists manager-native asset names
(wild: tree-relative paths); `theme` is optional (absent = unmanaged,
`""` = explicitly cleared).

Error/edge semantics: duplicate add → "already declared"; `--path` origin
is stored as the target verbatim (never re-resolved to a network URL);
unknown `enable` names are skipped and reported per row ("unknown spec
asset(s) skipped"); empty `enable` + no theme → managed block dropped,
tree kept.

## Env

`NIU_PLUGIN_SPEC` (spec path), `NIU_PLUGIN_SOURCES_ROOT` (install root),
`NIU_PLUGIN_BOOTSTRAP=off` (disable rc bootstrap line), `NIU_MIRRORS`
(mirror config path, default `~/.niubash/mirrors.json`).
