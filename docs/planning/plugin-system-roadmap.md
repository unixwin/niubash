# Plugin System Roadmap

This is the active execution plan for the Niubash-native plugin system. The
design direction is in [Plugin System Direction](plugin-system-direction.md).

## Current State

- The plugin registry, bundle inventory, permission review, doctor output, and
  enable/disable control plane exist.
- `oh-my-winuxsh` is the official bundled distribution.
- First-party packs are being moved behind manifests so their aliases,
  completions, prompt segments, hooks, and optional `.winux` startup code are
  visible and reviewable.
- `~/.niubashrc` is the user-authored interactive entry point.
- Manifest-backed registry files hold structured plugin state.

## Runtime Sequence

1. Keep `builtin` packs for host-owned behavior that still belongs in Rust.
2. Use asset-only packs for aliases, completions, themes, prompt presets, and
   keybinding metadata.
3. Use `source` packs for reviewed bundle-local `.winux` shell helpers.
4. Use `bridge` packs when the manifest exposes a host-owned capability.
5. Use `process` packs for external tool adapters that need explicit command
   permissions, timeouts, stdout/stderr capture, or interactive process launch.

## Near-Term Work

- Normalize every first-party pack manifest around exports and permissions.
- Ensure `niu plugin list/info/search/review/doctor` reports the same
  schema for builtin, source, bridge, and process packs.
- Move bundled completion and alias assets out of compiled defaults where the
  bundle can own them cleanly.
- Keep prompt layout theme-owned, with Git/Starship as segment providers.
- Add tests for plugin load order, user overrides, permission review, and
  missing-binary diagnostics.

## Pack Classification

| Pack shape | Runtime |
| --- | --- |
| Static aliases and completions | Asset-only manifest or `source` if functions are required. |
| Prompt layout themes | Theme asset pack. |
| Prompt data providers | `builtin` or `bridge` when host-owned, `process` when delegated to an external binary. |
| External command wrappers | `process` with explicit `process:run:<name>` permissions. |
| Shell-mutating helpers | Reviewed `.winux` `source` pack. |
| Core shell behavior | Niubash core, not a plugin. |

## Verification

- Unit tests for manifest parsing and inventory views.
- Runtime tests for source pack load order and user alias/function override
  behavior.
- CLI tests for plugin review/doctor output.
- Interactive smoke tests for prompt segment rendering, especially Git and
  Starship-backed Git.
