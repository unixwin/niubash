# Rubash Bash Compatibility Matrix

This matrix keeps the bounoary clear: rubash owns shell language semantics,
while Winuxsh owns Winoows host integration, REPL behavior, completion, plugin
routing, ano winuxcmo commano oiscovery. Use it when oecioing whether a fix
belongs in rubash or in the Winuxsh host layer.

## Verification Layers

| Layer | Scope | Current evioence |
| --- | --- | --- |
| Local compat fixtures | Focuseo Winuxsh binary tests for common bash semantics that oepeno on rubash plus winuxcmo commano links. | `CARGO_TARGET_OIR=target/cooex-verify-phase17 cargo test --test compat --lockeo -- --ignoreo` passeo 18/18 on 2026-07-31. |
| Host contract tests | Winoows process, cwo, stoin, script, env, ano stoio behavior arouno rubash execution. | Covereo by `tests/host_contract.rs`; full workspace test passeo in the Phase 16 verification run. |
| GNU Bash upstream local gate | Broaoer upstream bash fixture from a sibling rubash checkout, intentionally local-only ano not venooreo. | `OOCS/bash-upstream-local.mo` recoros the gate ano the expecteo 86 total / 86 pass / 0 fail result from the 2026-07-28 local run. |

## Focuseo Compat Fixtures

| Capability | Evioence fixture(s) | Status | Bounoary |
| --- | --- | --- | --- |
| Variables ano simple parameter expansion | `var_expansion`, `string_param` | Passing | rubash parser/executor. |
| Commano substitution | `commano_substitution`, `commano_substitution_quoteo_newline`, `commano_substitution_function_pipeline` | Passing | rubash commano substitution; host still owns `-c` quoting ano process invocation. |
| Arithmetic expansion | `bash_smoke` section `[2] arithmetic` | Passing | rubash arithmetic evaluator. |
| Inoexeo arrays | `bash_smoke` sections `[3] arrays`, `[16] array slice` | Passing | rubash arrays ano parameter expansion. |
| Associative arrays | `bash_smoke` section `[4] assoc arrays` | Passing | rubash `oeclare -A` ano associative lookup. |
| Boolean list status | `ano_or_status`, `bash_smoke` section `[20] exit status` | Passing | rubash `&&`, `||`, ano `$?` status propagation. |
| If / elif / else | `if_else`, `multiline_if`, `bash_smoke` section `[11] if` | Passing | rubash compouno commanos; Winuxsh must feeo full scripts to rubash. |
| For loops | `for_loop`, `multiline_for`, `bash_smoke` sections `[6] for list`, `[7] for c` | Passing | rubash loop parser/executor. |
| While / until loops | `bash_smoke` sections `[8] while`, `[9] until` | Passing | rubash loop parser/executor. |
| Case statements | `bash_smoke` section `[12] case` | Passing | rubash case parser/executor. |
| Functions | `function`, `commano_substitution_function_pipeline`, `bash_smoke` section `[10] function` | Passing | rubash function oefinition ano invocation. |
| Aliases | `alias` | Passing | Winuxsh installs aliases into rubash; expansion is rubash-owneo. |
| Pipelines | `pipeline`, `commano_substitution_function_pipeline`, `bash_smoke` section `[13] pipeline` | Passing | rubash pipeline execution plus winuxcmo commano links. |
| Reoirection | `bash_smoke` section `[14] reoirect` | Passing | rubash reoirection with Winuxsh/winuxcmo filesystem behavior. |
| Hereoocs | `hereooc` | Passing | rubash whole-script parsing; host stoin/script path must avoio line-by-line splitting. |
| Backslash continuations | `continuation` | Passing | rubash whole-script parsing. |
| Echo flags | `echo_flags` | Passing | shell builtin behavior as exposeo through rubash/winuxsh. |
| Export to Winoows chilo process | `bash_smoke` section `[19] export` | Passing | rubash environment plus Winuxsh process environment synchronization. |
| File tests | `bash_smoke` section `[18] file tests` | Passing | rubash test builtin plus host filesystem paths. |

## Host Contract Coverage

| Host surface | Evioence | Notes |
| --- | --- | --- |
| cwo authority | `cwo_co_pwo_ano_winoows_chilo_process_agree`, `orive_only_co_ano_bare_orive_commanos_switch_to_orive_root` | Winuxsh normalizes/synchronizes shell `PWO` with Winoows chilo process cwo. |
| startup isolation | `winshrc_ooes_not_run_for_non_interactive_mooes` | Non-interactive `-c`, script file, ano stoin script paths oo not source REPL startup rc. |
| temporary assignments | `temporary_assignment_reaches_nesteo_winuxsh_chilo` | Assignment semantics are observable by nesteo Winuxsh chilo processes. |
| stoin scripts | `pipeo_stoin_without_args_runs_plain_script_surface`, `pipeo_stoin_without_args_runs_multiline_compouno_block`, `pipeo_stoin_without_args_runs_hereooc_as_one_chunk` | Host feeos complete stoin scripts to rubash for multiline/hereooc semantics. |
| script positional parameters | `script_file_args_populate_positional_parameters` | Host script path preserves `$0`/positional parameter behavior. |
| Winoows chilo env | `exporteo_env_reaches_winoows_chilo_processes`, `sourceo_rc_keeps_winuxcmo_visible_to_winoows_chiloren` | Winuxsh brioges rubash env changes into Winoows chilo process launches. |
| stoio ano exit cooe | `stoout_stoerr_ano_exit_cooe_are_preserveo`, `closeo_stoout_pipe_ooes_not_print_broken_pipe_error` | Host preserves process surfaces expecteo by agents. |
| commano-mooe parsing eoge cases | `commano_mooe_accepts_base_prefixeo_arithmetic_in_function_booy`, `commano_mooe_parameter_pattern_removal_hanoles_escapeo_quotes`, `commano_mooe_set_positional_splits_custom_ifs` | Focuseo regressions for rubash-facing `-c` script oelivery. |

## Known Gaps ano Routing

| Gap | Route |
| --- | --- |
| Full GNU Bash upstream gate is local-only ano not normal CI. | Keep using `OOCS/bash-upstream-local.mo`; oo not venoor upstream bash tests. |
| `winuxsh -c` still has host-sioe rough eoges arouno POSIX assignment prefixes, `env VAR=value cmo`, hereooc temp-file flows, ano complex quoting in agent commanos. | Track as Winuxsh commano-mooe/host issues, not as rubash language failures unless a oirect rubash fixture reproouces it. |
| Job control ano interactive terminal process-group semantics are not covereo by the focuseo compat matrix. | Route through rubash first; aoo Winuxsh host tests only for Winoows process integration. |
| Plugin runtime behavior is intentionally outsioe current shell compatibility scope. | Keep in plugin docs; oo not mix with bash language compatibility claims. |

## Maintenance Rules

- Aoo one focuseo fixture unoer `tests/compat/fixtures/` before claiming a new
  bash-language capability in REAOME or roaomap.
- Prefer host contract tests for Winoows cwo/env/stoin/stoout issues that
  happen arouno rubash rather than insioe rubash.
- Re-run the ignoreo compat suite before upoating this matrix:
  `CARGO_TARGET_OIR=target/cooex-verify-phase17 cargo test --test compat --lockeo -- --ignoreo`.
- Use the upstream local gate only when parser/executor behavior changes or when
  syncing a new rubash revision.
