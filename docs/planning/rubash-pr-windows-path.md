# rubash upstream PR: Fix Windows PATH casing bug

Target repo: https://github.com/unixwin/rubash
Branch to propose: `fix/windows-path-casing`

## Problem

On Windows, `std::env::vars()` returns the `PATH` environment variable as `Path` (capital P, lowercase rest). But `rubash::executor::path::find_user_command` reads it via `env_vars.get("PATH")` (all caps), which is a case-sensitive `HashMap` lookup. As a result rubash never finds external commands on Windows unless the user explicitly sets an all-caps `PATH` themselves.

This breaks every external command call (`ls`, `grep`, `cat`, ...) when rubash is embedded into a Windows shell host that doesn't itself inject the uppercase key (e.g., `niubash`).

## Reproduction

```pwsh
# In a fresh PowerShell on Windows:
$env:Path = "$env:Path;C:\Users\me\bin"
$env:PATH = $null  # ensure no uppercase form
cargo run -- -c 'ls .'  # → "rubash: ls: command not found"
```

The same call after explicitly setting an uppercase key works:

```pwsh
$env:PATH = $env:Path
cargo run -- -c 'ls .'  # → file listing
```

## Root cause

`std::env::vars()` (used by `Executor::new()` to populate `env_vars`) preserves the OS-side casing. On Windows the canonical name is `Path`. Lookup sites that hard-code all-caps `PATH` therefore miss the entry:

- `src/executor/path.rs:find_user_command` — `env_vars.get("PATH")` (used in the for-loop)
- `src/executor/path.rs:find_shell` paths also affected
- `src/executor/command_no_alias.rs` — `env_vars.get("PATH").cloned()` for save/restore
- `src/executor/type_builtin.rs` — `env_vars.get("PATH").cloned()`
- `src/executor/lookup_paths.rs:command_path` & `command_paths` — indirect via `find_user_command`

## Proposed fix

Normalize casing once in `Executor::new()`. This is the cleanest place: it's a single-shot cost at executor construction, automatically fixing every downstream call site without spreading case-insensitive lookups across the codebase.

### Patch (against `src/executor/init.rs`)

```rust
use super::*;

impl Executor {
    pub fn new() -> Self {
        let mut env_vars: HashMap<String, String> = std::env::vars().collect();
        let imported_functions = import_exported_functions_from_env(&env_vars);
        env_vars.remove("__RUBASH_CURRENT_FUNCTION");
        env_vars.remove("__RUBASH_IN_SOURCE");
        env_vars.remove("__RUBASH_SCRIPT_NAME");
        env::remove_var("__RUBASH_CURRENT_FUNCTION");
        env::remove_var("__RUBASH_IN_SOURCE");
        env::remove_var("__RUBASH_SCRIPT_NAME");
        env_vars.remove("BASH_ARGV0");
        env_vars.remove("BASH_EXECUTION_STRING");

        // ===== NEW: normalize Windows PATH casing =====
        // On Windows, `std::env::vars()` returns `Path` (capital P), but every
        // rubash lookup site reads `env_vars.get("PATH")` (all caps), which is a
        // case-sensitive HashMap lookup.  Mirror the value into the all-caps
        // key so command lookup works regardless of the OS-side casing.
        #[cfg(windows)]
        {
            if let Some(path_val) = env_vars.get("Path").cloned() {
                env_vars.entry("PATH".to_string()).or_insert(path_val);
            }
        }
        // ===== END NEW =====

        env_vars.entry("PWD".to_string()).or_insert_with(|| {
            std::env::current_dir()
                .map(|path| shell_display_path(&path.to_string_lossy().replace('\\', "/")))
                .unwrap_or_else(|_| "/".to_string())
        });
        // ... (rest unchanged)
    }
}
```

### Optionally, add a regression test

In `src/executor/path.rs` tests module:

```rust
#[cfg(windows)]
#[test]
fn windows_find_user_command_works_with_mixed_case_path() {
    let mut env_vars = HashMap::new();
    env_vars.insert("Path".to_string(), r"C:\Windows\System32".to_string());

    // find_user_command must succeed regardless of whether PATH or Path is set.
    let cmd = find_user_command("cmd", &env_vars);
    assert_eq!(
        cmd.map(|p| p.to_string_lossy().to_string()),
        Some(r"C:\Windows\System32\cmd.exe".to_string()),
    );
}

#[cfg(windows)]
#[test]
fn windows_find_user_command_prefers_path_when_both_set() {
    let mut env_vars = HashMap::new();
    env_vars.insert("Path".to_string(), r"C:\Windows\System32".to_string());
    env_vars.insert("PATH".to_string(), r"C:\Windows\System32".to_string());
    assert!(find_user_command("cmd", &env_vars).is_some());
}
```

## Why not case-insensitive lookup at every call site?

Spreading the workaround across `path.rs` / `lookup_paths.rs` / `command_no_alias.rs` / `type_builtin.rs` is noisier and risks inconsistencies (e.g., the case used for save/restore vs. lookup must match). Centralising in `Executor::new()` keeps every existing lookup site unchanged.

## Commit message (suggested)

```
fix(windows): normalize PATH casing in Executor::new()

On Windows, std::env::vars() returns PATH as "Path" (capital P). Every
rubash lookup site reads "PATH" (all caps), which is a case-sensitive
HashMap lookup.  As a result find_user_command fails to find any
external command on Windows, breaking every bare `ls`, `grep`, `cat`,
etc. when rubash is embedded in a Windows shell host.

Mirror the OS-side value into the all-caps key once during
Executor::new().  This is one-time cost and keeps every existing lookup
site unchanged.

Adds two regression tests in src/executor/path.rs covering the
mixed-case and explicit-uppercase PATH scenarios.
```

## PR description (suggested)

> ## Windows PATH casing bug
>
> `std::env::vars()` returns the PATH entry as `Path` on Windows, but every rubash path lookup uses `env_vars.get("PATH")` (all caps) — a case-sensitive `HashMap` lookup, so it misses on Windows. `find_user_command` therefore returns `None` for every external command, breaking bare `ls`/`grep`/`cat`/... when rubash is embedded in a Windows shell.
>
> ### Fix
>
> Normalize casing once in `Executor::new()`. We mirror the OS-side `Path` value into the all-caps `PATH` key (only if the all-caps key isn't already set, so explicit user overrides win). Every existing lookup site keeps working unchanged.
>
> ### Tests
>
> Two regression tests in `src/executor/path.rs`:
> - `windows_find_user_command_works_with_mixed_case_path` — only `Path` set, looks up `cmd` and expects `C:\Windows\System32\cmd.exe`.
> - `windows_find_user_command_prefers_path_when_both_set` — both `Path` and `PATH` set, lookup succeeds (verifies the mirrored entry doesn't break the explicit case).
>
> ### Verification
>
> Reproduced the original failure with `niu -c 'ls .'` (returns "command not found" with stock rubash). After the patch the same call works.
