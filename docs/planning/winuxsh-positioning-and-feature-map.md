# Winuxsh Positioning and Feature Map

Winuxsh is a native Windows Bash-compatible shell, not a Linux environment and
not an emulation layer.

## Product Position

- Bash-compatible language surface through rubash.
- Native Windows process model and paths.
- Bundled Unix-style commands through WinuxCmd command links.
- Interactive shell UX through reedline.
- First-party plugin bundle through `oh-my-winuxsh`.

## Feature Ownership

| Area | Owner |
| --- | --- |
| Parser, executor, expansions, redirects, functions, pipelines | rubash |
| Windows cwd/env/PATH synchronization | Winuxsh |
| Unix-style command binaries and links | WinuxCmd |
| Line editing, history, menus | reedline + Winuxsh integration |
| Prompt rendering and theme selection | Winuxsh prompt-core and active theme |
| Git status provider | Winuxsh native provider or Starship-backed provider |
| Plugin manifests and bundle loading | Winuxsh |
| First-party aliases/completions/themes/helpers | oh-my-winuxsh |

## Plugin Shape

The plugin system should feel familiar to users of mature shell frameworks:
select named packs, load an official bundle, allow pack-local helper code, and
keep user customizations in a plain rc file. The implementation is
Winuxsh-native and should not expose another shell's plugin API as a contract.
