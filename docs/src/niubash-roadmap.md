---
tags: [niubash, roadmap]
created: 2026-07-13
status: active
---

# Niubash Roadmap

Niubash is a Windows-native, non-isolated Bash-compatible shell for humans and
agents.

```text
niubash = rubash + winuxcmd + reedline + oh-my-niu
```

## Done

- `rubash` is embedded as the shell language engine.
- WinuxCmd command links provide bundled Unix-style commands through PATH.
- The REPL uses reedline for editing, history, menus, autosuggestions, syntax
  highlighting, vi/emacs modes, and prompt rendering.
- `~/.niubashrc` is the primary interactive startup file.
- `~/.winshrc` is an old fallback only when `~/.niubashrc` is absent.
- Plugin CLI decisions, permissions, bundle versions, tests, and advanced
  overrides are maintained as internal managed state, not user configuration.
- Plugin inventory, review, doctor, enable/disable, update, and rollback
  surfaces exist.
- `oh-my-niu` is the official bundled plugin distribution.
- Git completions, Git prompt status, prompt templates, and p10-style segment
  presets are available.
- Host contract tests cover cwd, env, stdin, stdout/stderr, script args,
  command-mode parsing, and exit-code propagation.

## Current Direction

- Keep shell semantics in `rubash`; fix parser, executor, builtins, redirects,
  pipelines, functions, jobs, and Bash language behavior upstream.
- Keep Windows host integration in Niubash: cwd/env synchronization, PATH
  injection, installer behavior, command links, prompt rendering, and REPL UX.
- Model the plugin system after mature shell frameworks: named packs in a
  bundle, pack manifests, sourceable helper files, exported aliases,
  completions, functions, hooks, prompt segments, keybindings, and themes.
- Keep prompt layout user/theme owned. Providers such as native Git status or
  Starship-backed Git supply segment data; they do not replace the whole prompt
  unless the selected theme chooses that layout.
- Keep third-party integration on reviewed source packs and explicit process
  adapters until a real host API justifies another runtime.

## Near-Term Work

- Normalize first-party pack manifests around exports and permissions.
- Move more alias/completion/theme assets from compiled fallback into the
  bundled `oh-my-niu` distribution.
- Split prompt responsibilities clearly:
  - theme owns layout and connective text;
  - prompt-core renders templates;
  - Git/Starship providers supply data for `{git}` and related tokens.
- Add regression tests for installed bundle startup, prompt provider selection,
  and missing external binaries.
- File and fix WinuxCmd command issues separately from rubash language issues.

## Verification

- Fast loop: `cargo fmt --check -p niubash; cargo build --locked; cargo test --workspace --locked`
- Runtime library: `cargo test -p niubash-runtime --lib --locked`
- Host contract: run the ignored host suites with WinuxCmd command links in
  PATH when changing PATH, cwd, env, or command-link behavior.
- Local Bash upstream gate remains local-only and should report 86 total,
  86 passed, 0 failed for the Niubash binary under test.

## Locked Decisions

- License: MIT.
- `rubash` follows latest `unixwin/rubash` master.
- WinuxCmd stays integrated through PATH injection and command links.
- `~/.niubashrc` is the normal user-authored interactive config.
- Machine-managed state is not a human-authored configuration surface.
- `oh-my-niu` is the official bundle, not a fork of another shell
  framework.
