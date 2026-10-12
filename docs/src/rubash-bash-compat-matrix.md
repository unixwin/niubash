# Rubash Bash Compatibility Matrix

This matrix keeps the boundary clear: rubash owns shell language semantics,
while Niubash owns Windows host integration, REPL behavior, completion, plugin
routing, and winuxcmd command discovery. Use it when deciding whether a fix
belongs in rubash or in the Niubash host layer.

## Verification Layers

| Layer | Scope | Current evidence |
| --- | --- | --- |
| Local compat fixtures | Focused Niubash binary tests for common bash semantics that depend on rubash plus winuxcmd command links. | `CARGO_TARGET_DIR=target/codex-verify-phase17 cargo test --test compat --locked -- --ignored` passed 18/18 on 2026-07-31. |
| Host contract tests | Windows process, cwd, stdin, script, env, and stdio behavior around rubash execution. | Covered by `tests/host_contract.rs`; full workspace test passed in the Phase 16 verification run. |
| GNU Bash upstream local gate | Broader upstream bash fixture from a sibling rubash checkout, intentionally local-only and not vendored. | `docs/planning/bash-upstream-local.md` records the Niubash local gate; the current rubash upstream ledger is the rubash README "Compatibility at a Glance" section: 83 suites, 82 pass, 1 residual (nameref), re-run 2026-09-25 on rubash master 71c933eb. |

## Focused Compat Fixtures

| Capability | Evidence fixture(s) | Status | Boundary |
| --- | --- | --- | --- |
| Variables and simple parameter expansion | `var_expansion`, `string_param` | Passing | rubash parser/executor. |
| Command substitution | `command_substitution`, `command_substitution_quoted_newline`, `command_substitution_function_pipeline` | Passing | rubash command substitution; host still owns `-c` quoting and process invocation. |
| Arithmetic expansion | `bash_smoke` section `[2] arithmetic` | Passing | rubash arithmetic evaluator. |
| Indexed arrays | `bash_smoke` sections `[3] arrays`, `[16] array slice` | Passing | rubash arrays and parameter expansion. |
| Associative arrays | `bash_smoke` section `[4] assoc arrays` | Passing | rubash `declare -A` and associative lookup. |
| Boolean list status | `and_or_status`, `bash_smoke` section `[20] exit status` | Passing | rubash `&&`, `||`, and `$?` status propagation. |
| If / elif / else | `if_else`, `multiline_if`, `bash_smoke` section `[11] if` | Passing | rubash compound commands; Niubash must feed full scripts to rubash. |
| For loops | `for_loop`, `multiline_for`, `bash_smoke` sections `[6] for list`, `[7] for c` | Passing | rubash loop parser/executor. |
| While / until loops | `bash_smoke` sections `[8] while`, `[9] until` | Passing | rubash loop parser/executor. |
| Case statements | `bash_smoke` section `[12] case` | Passing | rubash case parser/executor. |
| Functions | `function`, `command_substitution_function_pipeline`, `bash_smoke` section `[10] function` | Passing | rubash function definition and invocation. |
| Aliases | `alias` | Passing | Niubash installs aliases into rubash; expansion is rubash-owned. |
| Pipelines | `pipeline`, `command_substitution_function_pipeline`, `bash_smoke` section `[13] pipeline` | Passing | rubash pipeline execution plus winuxcmd command links. |
| Redirection | `bash_smoke` section `[14] redirect` | Passing | rubash redirection with Niubash/winuxcmd filesystem behavior. |
| Heredocs | `heredoc` | Passing | rubash whole-script parsing; host stdin/script path must avoid line-by-line splitting. |
| Backslash continuations | `continuation` | Passing | rubash whole-script parsing. |
| Echo flags | `echo_flags` | Passing | shell builtin behavior as exposed through rubash/winuxsh. |
| Export to Windows child process | `bash_smoke` section `[19] export` | Passing | rubash environment plus Niubash process environment synchronization. |
| File tests | `bash_smoke` section `[18] file tests` | Passing | rubash test builtin plus host filesystem paths. |

## Host Contract Coverage

| Host surface | Evidence | Notes |
| --- | --- | --- |
| cwd authority | `cwd_cd_pwd_and_windows_child_process_agree`, `drive_only_cd_and_bare_drive_commands_switch_to_drive_root` | Niubash normalizes/synchronizes shell `PWD` with Windows child process cwd. |
| startup isolation | `winshrc_does_not_run_for_non_interactive_modes` | Non-interactive `-c`, script file, and stdin script paths do not source REPL startup rc. |
| temporary assignments | `temporary_assignment_reaches_nested_winuxsh_child` | Assignment semantics are observable by nested Niubash child processes. |
| stdin scripts | `piped_stdin_without_args_runs_plain_script_surface`, `piped_stdin_without_args_runs_multiline_compound_block`, `piped_stdin_without_args_runs_heredoc_as_one_chunk` | Host feeds complete stdin scripts to rubash for multiline/heredoc semantics. |
| script positional parameters | `script_file_args_populate_positional_parameters` | Host script path preserves `$0`/positional parameter behavior. |
| Windows child env | `exported_env_reaches_windows_child_processes`, `sourced_rc_keeps_winuxcmd_visible_to_windows_children` | Niubash bridges rubash env changes into Windows child process launches. |
| stdio and exit code | `stdout_stderr_and_exit_code_are_preserved`, `closed_stdout_pipe_does_not_print_broken_pipe_error` | Host preserves process surfaces expected by agents. |
| command-mode parsing edge cases | `command_mode_accepts_base_prefixed_arithmetic_in_function_body`, `command_mode_parameter_pattern_removal_handles_escaped_quotes`, `command_mode_set_positional_splits_custom_ifs` | Focused regressions for rubash-facing `-c` script delivery. |

## Known Gaps and Routing

| Gap | Route |
| --- | --- |
| Full GNU Bash upstream gate is local-only and not normal CI. | Keep using `docs/planning/bash-upstream-local.md` for the Niubash local gate; source rubash suite numbers from the rubash README compatibility ledger. Do not vendor upstream bash tests. |
| `niu -c` still has host-side rough edges around POSIX assignment prefixes, `env VAR=value cmd`, heredoc temp-file flows, and complex quoting in agent commands. | Track as Niubash command-mode/host issues, not as rubash language failures unless a direct rubash fixture reproduces it. |
| Job control and interactive terminal process-group semantics are not covered by the focused compat matrix. | Route through rubash first; add Niubash host tests only for Windows process integration. |
| Plugin runtime behavior is intentionally outside current shell compatibility scope. | Keep in plugin docs; do not mix with bash language compatibility claims. |

## Maintenance Rules

- Add one focused fixture under `tests/compat/fixtures/` before claiming a new
  bash-language capability in README or roadmap.
- Prefer host contract tests for Windows cwd/env/stdin/stdout issues that
  happen around rubash rather than inside rubash.
- Re-run the ignored compat suite before updating this matrix:
  `CARGO_TARGET_DIR=target/codex-verify-phase17 cargo test --test compat --locked -- --ignored`.
- Use the upstream local gate only when parser/executor behavior changes or when
  syncing a new rubash revision.
