---
tags: [niubash, roadmap]
created: 2026-07-13
status: active
---

# Niubash Roadmap

Niubash is a Windows-native, non-isolated Bash-compatible shell for humans and
agents.

```text
niubash = rubash + winuxcmd + reedline + the external bash ecosystem
```

## Done

- `rubash` is embedded as the shell language engine, pinned to verified
  upstream commits (v1.4.3 vendors the Bash-5.3 patch-20 surface).
- WinuxCmd command links provide bundled Unix-style commands through PATH.
- The REPL uses reedline for editing, history, menus, autosuggestions, syntax
  highlighting, vi/emacs modes, and prompt rendering.
- `~/.niubashrc` is the primary — and only — interactive startup file; the
  legacy `~/.winuxshrc`/`~/.winshrc`/`~/.bashrc` names are never read.
- The plugin system is the external bash ecosystem: real sources
  (oh-my-bash, bash-it, bash-completion, wild git repos) installed over
  `git clone`, driven by the declarative
  manifest `~/.niubash/plugins.toml`, and activated through the generated
  loader `~/.niubash/generated/plugins.niu` — no verb writes the user rc
  (niubash#359, v1.4.2).
- Plugin inventory, review, doctor, enable/disable, update, and rollback
  surfaces exist, plus a ~540-row recipe index and collections
  (`niu plugin distro`).
- Git completions, Git prompt status, prompt templates, and p10-style segment
  presets are available.
- Host contract tests cover cwd, env, stdin, stdout/stderr, script args,
  command-mode parsing, and exit-code propagation.
- Released and smoke-gated beyond Windows: Linux (x86_64, aarch64) and macOS
  (aarch64, x86_64) portable builds ship beside the Windows installer.

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
- Keep third-party integration on reviewed sources and explicit process
  adapters until a real host API justifies another runtime.

## Near-Term Work

- Grow the source coverage of the plugin ecosystem: more catalog managers,
  recipes, and collections riding the manifest + generated-loader plane.
- Land the remaining agent-facing installer paths named in
  [agents.md](agents.md#recommended-order-of-work) — persisted `SHELL`,
  `niu setup agents`, and an accepted opt-in elevated task for the Git Bash
  convention slot.
- Re-measure the Android baseline post-fix (niubash#352) and move the Android
  leg from build-verified toward smoke-gated.
- Split prompt responsibilities clearly:
  - theme owns layout and connective text;
  - prompt-core renders templates;
  - Git/Starship providers supply data for `{git}` and related tokens.
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
- `~/.niubashrc` is the normal user-authored interactive config — and the
  only rc file the shell reads.
- Machine-managed state is not a human-authored configuration surface.
- The official plugin story is the upstream ecosystem itself — oh-my-bash,
  bash-it, bash-completion — installed on demand, never
  vendored into the product.
