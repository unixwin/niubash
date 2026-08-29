# Niubash v3 Plan

Niubash v3 is about making the shell feel complete as a Windows-native Bash
environment while keeping extensions maintainable.

## Architecture

```text
niubash = rubash + winuxcmd + reedline + oh-my-winuxsh
```

- `rubash` owns shell language semantics.
- `winuxcmd` owns bundled Unix-style commands.
- `reedline` owns interactive editing primitives.
- Niubash owns Windows host integration, prompt rendering, plugin loading, and
  configuration.
- `oh-my-winuxsh` owns first-party plugin assets and reviewed `.winux` helper
  scripts.

## Configuration

- `~/.niubashrc` is the primary interactive startup file.
- `~/.winshrc` remains an old fallback only when `~/.niubashrc` does not exist.
- Plugin CLI operations, bundle versions, tests, and advanced overrides use
  the manifest-backed registry.

## Plugin Model

- Load named packs from `oh-my-winuxsh`.
- Keep pack manifests auditable.
- Use asset packs for aliases, completions, themes, and keybinding metadata.
- Use `.winux` source packs for reviewed shell mutation.
- Use process adapters for external tools.
- Keep prompt layout owned by the selected theme; segment providers only supply
  data.

## Non-Goals

- No second shell language runtime.
- No arbitrary rc-file sourcing as plugin code.
- No plugin access to parser/executor internals.
- No long-term host-side workaround for rubash language bugs.
