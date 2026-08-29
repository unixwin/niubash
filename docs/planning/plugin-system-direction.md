# Plugin System Direction

This note is the authoritative direction for the Niubash plugin system.
The execution sequence lives in [Plugin System Roadmap](plugin-system-roadmap.md).

Niubash plugins are Niubash-native. The system follows the useful operating
model of mature shell plugin ecosystems: a small framework loads named packs
from a bundle, each pack owns aliases, completions, functions, prompt segments,
hooks, and optional startup code, and user configuration remains plain shell
script.

## Decisions

- `~/.niubashrc` is the primary interactive entry point for plugin lists, theme
  selection, prompt templates, exports, aliases, functions, and startup logic.
- `oh-my-winuxsh` is the official bundled plugin distribution. It is not a fork
  of another shell framework.
- `~/.winshrc` remains a compatibility fallback only when `~/.niubashrc` is
  absent.
- The manifest-backed registry stores structured plugin CLI state for
  enablement, permissions, bundle versions, tests, and advanced
  machine-editable overrides.
- Plugin manifests are the control plane; `.winux` source files are the trusted
  script plane for shell-mutating bundle code.
- Prompt ownership stays with the active user/theme template. Segment providers
  such as Git or Starship may provide values for template tokens, but they do
  not replace the user's whole prompt unless the user selects a theme that does
  so.

## Product Model

```text
niubash core
  - rubash shell execution
  - reedline interactive frontend
  - config loading
  - plugin registry
  - permission model
  - bundle update / rollback plumbing

oh-my-winuxsh bundled distribution
  - official first-party plugin manifests
  - .winux source scripts
  - aliases, completions, functions, prompt presets, prompt segments
  - keybinding presets and theme assets
  - independent version and update channel

external integrations
  - source packs for reviewed, trusted shell startup code
  - process adapters for external tools and debug bridges
```

## Runtime Kinds

Plugins share one manifest and one permission model, with runtime kinds chosen
for the actual behavior they need:

| kind | Purpose |
| --- | --- |
| `builtin` | Host-owned Rust implementations that remain part of Niubash core. |
| `source` | Bundle-local `.winux` startup scripts sourced into the interactive shell. |
| `bridge` | Host-provided adapter surface for a pack that delegates to core features. |
| `process` | External tool adapters with explicit command permissions and timeouts. |

Static assets such as aliases, completions, themes, prompt templates, and
keybinding metadata do not need a code runtime. Shell-mutating helpers should
use `source` only when that mutation is the product requirement. External tools
such as `direnv`, `fzf`, or correction providers should use `process` or a
host-owned `bridge`, depending on where the behavior belongs.

## OMZ-Style Boundaries

The framework should be structured around small, auditable packs:

- A pack has a manifest.
- A pack can export aliases, completions, functions, prompt segments, hooks,
  commands, keybindings, themes, and settings.
- A pack that needs shell code has one reviewed `.winux` entry point.
- A theme owns prompt layout and connective text.
- Segment providers produce data only. For example, the Git segment can be
  native or Starship-backed, while the theme still decides where `{git}` is
  rendered and what surrounds it.
- User startup remains writable shell code in `~/.niubashrc`; TOML records
  managed decisions and permission grants.

## Minimum Manifest Surface

```toml
name = "git"
bundle = "oh-my-winuxsh"
version = "1.0.0"
kind = "source"
api = "niubash:plugin@0.1.0"
summary = "Git aliases, completions, and prompt segments."
permissions = ["shell:source", "cwd:read", "process:run:git"]

[exports]
aliases = true
completions = ["git"]
prompt_segments = ["git"]
hooks = ["startup"]
commands = []

[source]
entry = "packs/git/init.winux"

[settings]
show_dirty = true
show_ahead_behind = true
```

## Implementation Order

1. Keep the current plugin CLI and manifest registry focused on `[plugins]`.
2. Load the bundled `oh-my-winuxsh` baseline from the installed bundle path.
3. Move first-party data assets into the bundle where safe.
4. Use `.winux` source packs for reviewed shell helpers.
5. Keep core shell machinery in Niubash: rubash integration, reedline
   primitives, cwd/env/path synchronization, prompt rendering, and native
   builtins.
6. Use process adapters only for external tools that must run as processes.

## Non-Goals

- No plugin access to rubash parser/executor internals.
- No arbitrary user-discovered rc fragments as plugin code.
- No DLL/FFI plugin ABI for community plugins.
- No plugin behavior that depends on executing fallback `~/.winshrc`.
