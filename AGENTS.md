# Niubash Agent Rules

## Command Runner

- Do not use PowerShell or pwsh as the project command language.
- If the active terminal/tool session is already Niubash, run project commands directly. Do not wrap ordinary work in `niu -c`.
- Use `niu -c '<command>'` only when launching Niubash from an external host process, or when deliberately testing fresh script-mode semantics.
- Ensure `niubash` resolves to the intended installed user binary, usually from `PATH` or a user tools directory outside this repository such as `~/tools`. Do not commit developer-local absolute runner paths.
- Do not let `PATH` resolve `niubash` to this repository's `target/release` or `target/debug` unless deliberately testing that exact build; those binaries can be stale or locked during builds.
- Use checked-out binaries only when deliberately testing that exact build, for example `target/debug/niu.exe --version` after `cargo build`.
- Keep commands Windows-native. Use normal Windows paths (`C:/Users/<user>/...` or `C:\Users\<user>\...`) and do not introduce MSYS2, Git Bash, Cygwin, or WSL assumptions.
- If `ls`, `grep`, `tr`, or similar Unix commands are missing, fix the winuxcmd command-link setup rather than switching shells. A release-style bundle needs `winuxcmd.exe` plus generated command links in `PATH`.

## Product Direction

- Niubash is a Windows-native, non-isolated bash-compatible shell for humans and agents.
- Rubash is the shell language engine and is embedded as a library. Parser, executor, builtins, functions, redirects, pipelines, and job semantics belong upstream in `unixwin/rubash`.
- Keep rubash on the latest `unixwin/rubash` `master`. We have Unixwin organization access, so fix rubash upstream instead of carrying long-term host-side semantic workarounds in niubash.
- WinuxCmd stays integrated through PATH injection and command links. Do not reintroduce FFI/DLL command routing.
- The plugin system is Niubash-native and built into niubash. `oh-my-winuxsh` is the official bundled plugin distribution.
- Use `~/.niubashrc` as the primary interactive user entry point for plugin lists, theme selection, exports, aliases, functions, and shell startup logic. Fallback order: `~/.winuxshrc` (pre-rename compat; migrated once into `~/.niubashrc` on first startup, original file kept) then `~/.winshrc` (legacy fallback, read only when `~/.niubashrc` is absent).
- Runtime environment variables use the `NIU_` prefix. During the transition, niubash also sets/reads the deprecated `WINUXSH_SHELL`, `WINUXSH_SHELL_PATH_STYLE`, and `WINUXSH_ROOT` names because current rubash upstream still reads them; these bridges are marked with comments and must be removed once rubash renames its readers.
- Keep structured plugin and bundle assets in their manifest-backed TOML files; user startup configuration belongs in `~/.niubashrc`.
- Use the manifest-backed registry as the control plane for every runtime. Use `source` packs with bundle-local `.winux` scripts for reviewed shell helpers, keep `builtin` for host-owned native behavior and fallback, and keep process plugins for external-tool adapters and debug bridges.

## Development Rules

- Preserve quiet, deterministic non-interactive behavior for direct Niubash project commands, `niu -c`, and script execution: no banners, stable stdout/stderr, exact exit-code propagation.
- Keep interactive UX features in niubash/reedline unless they require shell semantics; shell semantics move to rubash.
- Keep compatibility tests honest: `tests/compat.rs` requires winuxcmd command links in the Windows process `PATH`, not just a bare `winuxcmd.exe`. When prepending a local WinuxCmd build for `cargo test`, use the Windows separator (`;`), for example `PATH="C:/path/to/WinuxCmd/build-vs-release;$PATH"`.
- Keep one authoritative dependency lock at the repository root. Do not let `crates/niubash-runtime/Cargo.lock` drift from the binary build.
- When changing rubash, update the root lockfile and verify niubash through the root package.
- Never run destructive git operations against the rubash working tree. If the rubash checkout shows any uncommitted changes (modified, staged, or untracked files), treat it as someone else's in-progress work. Prohibited commands include git stash, git stash push, git reset, git checkout that discards edits, git restore, git clean, and any checkout or switch that would hide local edits. When rubash's local state blocks your build or verification, stop and report the blocker to the user. Do not stash, reset, restore, clean, or rewind the rubash tree to make it compile.

## Verification

- Fast loop: `cargo fmt --check -p niubash; cargo build --locked; cargo test --workspace --locked`
- Rename brand gate: `sh scripts/check-rename-clean.sh` must pass (no unexpected `winuxsh` residue; runs under Git Bash and `winuxsh -c` alike).
- Runtime library: `cargo test -p niubash-runtime --lib --locked`
- Host contract requiring winuxcmd command links: ensure command links are in the Windows process `PATH`, then run `PATH="C:/path/to/WinuxCmd/build-vs-release;$PATH" WINUXCMD_PATH="C:/path/to/WinuxCmd/build-vs-release/winuxcmd.exe" cargo test --test host_contract --locked -- --ignored`
- Compat suite: ensure command links are in the Windows process `PATH`, then run `PATH="C:/path/to/WinuxCmd/build-vs-release;$PATH" WINUXCMD_PATH="C:/path/to/WinuxCmd/build-vs-release/winuxcmd.exe" cargo test --test compat --locked -- --ignored`
- Local GNU Bash upstream gate: `BASH_RUNNER="${BASH_RUNNER:-bash}"; "$BASH_RUNNER" scripts/run-bash-upstream-with-niubash.sh` must report `86` total, `86` passed, `0` failed for the Niubash binary under test. If `bash` is not in Niubash's `PATH`, run the same gate with an explicit Git Bash path such as `C:/Progra~1/Git/bin/bash.exe`. Keep this local-only; do not add it to normal CI, and do not vendor Bash upstream tests into this repo. See `docs/planning/bash-upstream-local.md`.
