# Winuxsh Next Development Plan

## Focus

- Stabilize Winuxsh as a Windows-native Bash-compatible shell.
- Keep shell semantics in rubash and host behavior in Winuxsh.
- Make `oh-my-winuxsh` the maintained first-party plugin bundle.
- Keep configuration understandable: user script in `~/.winuxshrc`, managed
  state in the manifest-backed plugin registry.

## Workstreams

1. Host contract hardening: cwd, env, PATH, command links, stdin/stdout/stderr,
   exit status, and script argument behavior.
2. Plugin bundle hardening: manifests, permissions, review output, doctor
   diagnostics, update/rollback, and installed bundle discovery.
3. Prompt hardening: theme-owned layout, prompt-core rendering, native Git
   provider, optional Starship-backed Git provider.
4. Completion hardening: bundle-owned static tables, user override ordering,
   runtime completion adapters, and cache invalidation.
5. Installer hardening: reliable PATH setup, WinuxCmd activation, command-link
   repair, and Windows Terminal profile creation.

## Reference Policy

Reference other shells for proven interaction patterns, but do not promise
runtime compatibility with their configuration language, plugin API, or editor
internals. Winuxsh should borrow the framework shape that works: named packs,
small startup scripts, prompt/theme separation, and user-owned shell config.
