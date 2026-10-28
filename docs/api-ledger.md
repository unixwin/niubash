# API Ledger — the engine surface niubash consumes

> This file is checked into **both** repositories (rubash and niubash)
> at `docs/api-ledger.md`. The two copies are one document: they must
> stay byte-identical, and the pairing CI on both sides enforces it
> (see §5). Edit one copy, mirror the edit to the other in the same
> change, or the canary goes red.
>
> Status: M-0 ledger for the 1.5.0 Session API RFC (rubash#595;
> in-repo design record: `docs/design/1.5.0-session-api.md` in rubash).
> This ledger is the mechanical form of that RFC's §3.2 consumer census
> and §9 step M-0. During the 1.5.0 workspace split, the surface listed
> here is **frozen** in the RFC's terms: no listed symbol may be
> removed, renamed, or re-signed without a ledger update in the same
> change and a tier assignment for whatever replaces it (§4).
>
> Ledger v3 (1.5.0 Session contract publication, 2026-10-28): the
> RFC's Tier A contract is now **published in-tree** as `rubash::api`
> (full contract in 1.5.0; niubash migrates onto it inside the same
> release; the workspace split stays post-release and will move the
> module to the split core crate unchanged in shape). §5 gains the
> completeness half of the tripwire: the recorded symbols' existence
> is no longer the only check — everything niubash consumes must be
> recorded here (§0 states the v3 census basis).

## 0. Census basis and counting rules

Measured on 2026-10-25 against:

- rubash master `abd9be95b556556149ab44c45d0e80d27eefa8f6`
- niubash master `b53e6bffcfa4abe25a1e2176b16b63cdc24f487b`

Re-census for Ledger v2 (PLAN row 11, 2026-10-28), same rules:

- rubash master `231ffe1a1b28923ce60c38552ed7f5ad5e99e6ea`
  (the row-11 branch base; the row's rubash PR is its child)
- niubash master `c06b2a08` (PR #624 merge)

Path references at the re-census: **133 inline occurrences, 65
distinct paths** after the use-list union, spread over **7
files** (the basis set plus
`crates/niubash-runtime/src/syntax_highlighting.rs`). Symbols
niubash gained since the basis — `ExecuteError::SigintUnwind`,
`script_driver::stdin_script_errexit_enabled`,
`executor::read_process_stdin_bytes`,
`executor::bytes_to_shell_text`, and the methods
`take_signal_discard_pending`, `mark_login_shell`,
`last_command_inverted` — are recorded in §6 with this census
as their basis. The row-11 dispositions (§4) move the
`builtins::kill` hooks out of the consumed surface and add
their replacements.

Re-census for Ledger v3 (Session contract publication, 2026-10-28),
same rules:

- rubash master `45a0973f3e997c1f2c6591fc375d47086ffc39d2`
  (the publication branch base)
- niubash master `f34d6634652e0404acb9b74d2c5f9359e92bec20`

Path references at the re-census: **76 distinct paths** after the
use-list union. The v2 census missed two consumed families — the
debugger front doors (the root re-exports `run_dap_stdio`,
`run_debug_cli`, `DapLaunchPreset`, `DebugCliPreset`) and the
`builtins::bind` spec helpers niubash's bind surface calls — plus
three single items (`shell::bind_registry::KEYMAP_NAMES`, `Ast`
under its defining-module spelling `parser::Ast`, and the method
`history::SessionHistory::size_limit`); all are recorded in §6
with this census as their basis, and §5's completeness check now
makes a fourth miss mechanically impossible to ship. One v2 entry
is removed: `script_driver::run_script_with_history` is no longer
referenced anywhere in niubash (the tracked driver carries that
traffic, and `Session::run` above it), so it leaves the consumed
surface; the symbol itself stays `pub` in the engine.

Counting rules (so any future re-census is comparable):

- **Path references**: occurrences of the token pattern
  `rubash::[A-Za-z0-9_:]+` in `*.rs` under niubash `crates/` and `src/`
  (all files, including in-file test modules), unioned with the items
  named in `use rubash::{...}` lists. At the basis: **118 inline
  occurrences, 59 distinct inline paths, 61 distinct paths after the
  use-list union**, spread over exactly **6 files**:
  `crates/niubash-runtime/src/{shell,repl,history,ctrl_c}.rs`,
  `crates/niubash-runtime/src/completion/command.rs`, `src/main.rs`.
- **Method calls**: occurrences of `executor.<name>(` where the
  receiver is niubash's held `rubash::Executor` (the `Shell::executor`
  field, locals, and `&mut Executor` parameters in the files above),
  unioned with `Executor::<name>` associated calls.
- **Engine surface**: counted from rubash `src/lib.rs` and `src/`
  declarations at the basis commit (rules stated per figure below).

## 1. The engine surface today (rubash side)

At the basis commit, `src/lib.rs` declares **18 public modules**
(`grep -c '^pub mod' src/lib.rs`):

`build_info`, `builtins`, `console_readline`, `executor`, `expand`,
`history`, `history_expand`, `invocation`, `jobs`, `lexer`, `locale`,
`parser`, `posix_errors`, `proc_vfs`, `script_driver`, `shell`,
`token_str`, `usage` — plus one `pub(crate)` module (`fd`). There is no
`#[doc(hidden)]` separation and there are no feature gates.

Root re-exports (8 unconditional): `Executor`, `ExecuteError`
(from `executor`); `Token`, `TokenKind` (from `lexer`);
`decode_to_visible_text` (from `locale`); `Ast`, `CommandNode`,
`Redirect` (from `parser`). Windows-only root re-exports (3):
`ElevationOutput`, `ElevationRequest`, `SudoMode`.

`Executor`'s public methods are spread across the `src/executor/`
tree: **121 distinct `pub fn` names / 125 declaration sites**
(`grep -rhoE 'pub fn [a-z_0-9]+' src/executor/`), 61 of the declaration
sites in `public_accessors.rs` alone. Nothing marks which of these are
contract and which are plumbing — that is the problem this ledger and
the 1.5.0 tiers exist to fix.

> Census drift note: the RFC's §3.1 says "20 public modules". That
> figure does not reproduce at the basis commit (18 `pub mod` + 1
> `pub(crate)`). The ledger records the measured figure; the RFC text
> is design record and is not rewritten retroactively.

## 2. What niubash consumes (the compatibility baseline)

### 2.1 Path references by family (distinct paths, with occurrences)

| Family | Items (occurrences) |
| --- | --- |
| `executor` | `Executor` (type; +§2.2 methods), `ExecuteError` (2) + variants `ExitCode` (6), `Return` (4), `CommandNotFound` (3), `ExpansionFailure` (3), `FatalFunctionError` (3), `IoError` (1); `shell_pwd_display_path` (1) |
| root re-exports | `Executor` (1), `Token` (1), `TokenKind` (1) + variants `Word`, `Assignment`, `BraceExpand`, `CommandSubst` (1 each), `Ast` (use-list), `decode_to_visible_text` (1) |
| debugger front doors | `run_dap_stdio`, `run_debug_cli`, `DapLaunchPreset`, `DebugCliPreset` — root re-exports consumed by niubash's `--dap` / `--debug` entry points (missed by the v2 census; recorded in v3) |
| `builtins::bind` | `BindingSpec` + variants `Function`, `Macro`, `BindSpecError`, `format_invalid_keymap`, `parse_positional_binding`, `parse_unix_command_spec`, `strip_set_keyword` — the bind builtin's spec-parsing helpers, consumed inside niubash's gated bind surface (missed by the v2 census; recorded in v3) |
| `history` | `HistoryProvider` (7) — trait impl + `SharedHistoryProvider` injection; the one true capability trait today; plus the `SessionHistory::size_limit` associated helper (recorded in v3) |
| `script_driver` | `run_source_with_line_offset` (2), `run_stdin_script`, `run_script_with_history`, `run_script_with_history_at_line_offset_tracked`, `run_interactive_stdin`, `pre_process_interactive_line`, `prepare_interactive_history`, `script_uses_history`, `script_uses_aliases`, `check_binary_file`, `bytes_to_script_text`, `finish_shell`, `heredoc_delimiter_unclosed_quote`, `READ_STDIN_MARKER`, `InteractiveExpansion` + variants `Execute`, `Discard` (1 each unless noted) |
| `parser` | `CommandNode` (14), `parse` (1), `parse_with_options` (1), `ParseLoopOptions` (1), `WordMetadata` (+ `literal`, §2.2), `ast_print::pretty_print_command` (1); `Ast` is also consumed under this defining-module spelling (the root re-export above is the other spelling; both recorded in §6) |
| `lexer` | `tokenize` (2), `tokenize_with_initial_posix` (1), `has_unclosed_input_syntax` (1), `has_unclosed_input_syntax_posix` (1) |
| `shell::bind_registry` | `BindEntry` (10), `BindKind` + variants `Execute` (6), `Function` (3), `Macro` (2) — 23 references at the v2 re-census, all of them inside niubash's gated bind surface since row 11 (see §4); plus `KEYMAP_NAMES` (recorded in v3) |
| `invocation` | `ShellInvocation` (3, + `parse`, §2.2), `materialize_command_file` (1) |
| `usage` | `LONG_OPTIONS` (3), `SHORT_OPTIONS` (2), `LONG_OPTIONS_WITH_VALUE` (1), `show_shell_version` (1), `show_shell_help` (1), `is_short_option_letter` (1), `invocation_help_name` (1), plus 2 module-level references |
| `builtins::kill` | **exited in row 11 (M-3)**: `note_console_ctrl_c` (2) and `consume_background_wrapper_handshake` (1) are no longer consumed — niubash calls the Tier B driver faces `script_driver::inject_console_ctrl_c` / `script_driver::consume_background_wrapper_handshake` instead, and the Tier A replacements are `Executor::inject_signal` / `Executor::interrupt`. The kill items remain `pub` in the engine only until niubash's migration is on master (the pairing compile builds niubash master against this tree); their demotion to `pub(crate)` is the recorded follow-up below. (One of these hooks disappearing is what broke niubash master in the bulk-merge incident — which is why the exit is staged, not abrupt.) |

### 2.2 `Executor` methods niubash calls (receiver-verified)

38 distinct methods. Occurrence counts by the §0 rule; the RFC §3.2
top set (`set_env` ×55, `set_shell_option` ×22, `last_exit_code` ×11,
`set_shopt_option` ×7, `inherit_process_stdin` ×5,
`set_history_provider` ×1, `complete_line` ×1, counted there as raw
`.name(` occurrences across `crates/` + `src/`) reproduces exactly
under that rule at the basis commit.

| Method | Calls | Method | Calls |
| --- | --- | --- | --- |
| `get_env` | 84 | `set_positional_params` | 3 |
| `set_env` | 49 | `has_compspec` | 3 |
| `set_shell_option` | 22 (+1 assoc) | `execute_ast` | 3 |
| `last_exit_code` | 11 | `bind_registry_generation` | 3 |
| `set_shopt_option` | 6 | `functions_snapshot` | 2 |
| `unset_env` | 6 | `export_env` | 2 |
| `set_last_exit_code` | 6 | `env_vars_snapshot` | 2 |
| `inherit_process_stdin` | 5 | `call_function_with_env` | 2 |
| `resolve_shell_path` | 4 | `take_parse_error` | 1 |
| `expand_prompt_string_mut` | 4 | `take_exit_jump_pending` | 1 |
| `alias_expansion_enabled` | 4 | `set_winuxcmd_path` | 1 |
| `resolve_shell_path_from_env` (assoc) | 3 | `set_shell_root` | 1 |
| `builtin_command_names` (assoc) | 2 | `set_compatible_shell_path` | 1 |
| `new` (assoc) | 2 | `set_builtin_disabled` | 1 |
| `set_history_provider` | 1 | `run_exit_trap_with_status` | 1 |
| `complete_line` | 1 (+1 assoc) | `has_function` | 1 |
| `resolve_shell_path_process_entries_from_env` (assoc) | 1 | `find_script_on_path` | 1 |
|  |  | `execute_prompt_command` | 1 |
|  |  | `execute_command` | 1 |
|  |  | `bind_registry_snapshot` | 1 |
|  |  | `aliases_snapshot` | 1 |

Two further non-`Executor` methods are consumed:
`ShellInvocation::parse` (2) and `WordMetadata::literal` (1).

Note what the receiver-verified count surfaces that the RFC's raw
count did not: the single most-called `Executor` method is the read
side, `get_env` (84). The RFC §8 mapping already assigns it to the
Tier A variables view; the working set a designed API must put first
is read *and* write, not write alone.

## 3. Alignment with the RFC census (§3.2)

Re-measured line by line at the basis commits, the RFC's §3.2 table
holds: every family it lists is present with matching membership;
the bind-registry family totals exactly the 20 references it claims;
its seven method counts reproduce exactly under its counting rule.
Additions this ledger makes, without contradicting it: the use-list
union (brings in `Ast` and the `Executor` type itself), the
receiver-verified full method set (§2.2), and the §1 drift note on
the module count.

## 4. Tier assignment and target homes

Per RFC §8 (mapping) — recorded here so a frozen symbol's destination
is a ledger fact, not a later guess. During the split (step S2 of the
RFC), Tier C items niubash touches stay reachable through the facade
or the gated internals feature until their migration step (§9 M-3 of
the RFC) lands.

Publication status (Ledger v3, 1.5.0): the Tier A homes in this
table have **landed** — `rubash::api` is a public module in-tree,
carrying `Session` / `SessionBuilder`, `RunOptions` / `RunOutcome` /
`Flow`, the §5.3 state views, the capability traits (the history
provider among them, unchanged per the RFC), the observer trait,
and the tier-B syntax views of design Q4. niubash's migration onto
them is the release's second step and proceeds consumer-file by
consumer-file against this ledger; until a consumer file migrates,
its §6 symbols stay frozen exactly as recorded. The workspace
split (PLAN row 4) remains post-release: it moves `rubash::api`
to the split core crate without changing its shape.

| Consumed family | Tier | Target home |
| --- | --- | --- |
| `Executor` lifecycle + `new` | A | `Session::builder()` |
| env methods (`get_env`, `set_env`, `unset_env`, `export_env`, `env_vars_snapshot`) | A | `Session::variables()` view |
| option methods (`set_shell_option`, `set_shopt_option`) | A | `Session::options()` view |
| `last_exit_code` / `set_last_exit_code` | A | `Session::last_status` + `RunOutcome` |
| `complete_line`, `has_compspec` | A | `Session::complete` + compspec view |
| `set_history_provider` + `HistoryProvider` | A | builder-injected capability (trait unchanged) |
| `call_function_with_env`, `has_function`, `functions_snapshot` | A | `Session::call_function` + functions view |
| `inherit_process_stdin` | A | builder stream binding |
| `execute_ast`, `execute_command`, `execute_prompt_command`, `run_exit_trap_with_status`, `expand_prompt_string_mut`, `set_positional_params`, `take_parse_error`, `take_exit_jump_pending` | A (run path) | `Session::run` + `RunOptions` / `RunOutcome` |
| `script_driver` run family | A (driver wrappers B) | `Session::run`; thin wrappers stay in the driver crate |
| `pre_process_interactive_line` + `InteractiveExpansion` | A | `Session::prepare_line` |
| completeness (`has_unclosed_input_syntax*`, `script_uses_*`, `check_binary_file`, `heredoc_delimiter_unclosed_quote`, `READ_STDIN_MARKER`) | A / B | `Session::needs_more_input`; helpers in `api::syntax` |
| `usage` printers and option tables | B | stay the shared host/engine bridge |
| `parser` family (`parse*`, `CommandNode`, `ParseLoopOptions`, `WordMetadata`, `ast_print`) | B | `Session::parse` + `api::syntax` view types (no internal `Ast` in Tier A) |
| `lexer` family (`tokenize*`) | B | `api::syntax` token view |
| `shell::bind_registry` (`BindEntry`, `BindKind`) | C | interactive layer's own gated surface — **landed as gated in row 11**: niubash consumes it only inside `crates/niubash-runtime/src/bind_surface.rs`, behind niubash's `internals` feature (default on; a `--no-default-features` build has zero Tier C references), converted there to niu-owned mirror types |
| `invocation` (`ShellInvocation`, `materialize_command_file`) | B (row-11 ruling) | driver-layer input type: both binaries' `main()` parse through it identically, and the RFC's own §8 puts driver wrappers at B; it stays in the `invocation` module in-tree and moves with the driver layer at the split |
| `builtins::kill` console hooks | C → **replaced (row 11)** | `Executor::inject_signal` / `Executor::interrupt` (Tier A, RFC §6.4) for session-scoped delivery; `script_driver::inject_console_ctrl_c` / `script_driver::consume_background_wrapper_handshake` (Tier B) for the two host-launcher shapes that have no executor yet. The kill items themselves are demoted to `pub(crate)` in the follow-up change once niubash's migration is on master |
| session configuration (`set_winuxcmd_path`, `set_shell_root`, `set_compatible_shell_path`, `set_builtin_disabled`, `mark_login_shell`) | A (row-11 ruling) | builder-time session initialization — M-2's own language ("the 55 set_env sites collapse into builder initialization"); these five are the same shape: host facts installed before the first run |
| driver path resolution (`resolve_shell_path`, `resolve_shell_path_from_env`, `resolve_shell_path_process_entries_from_env`, `find_script_on_path`, `shell_pwd_display_path`) | B (row-11 ruling) | driver helpers beside `script_driver`: the host/driver layer's path machinery, consumed identically by both binaries' startup and script lookup |
| signal/run state accessors (`take_signal_discard_pending`, `last_command_inverted`, `ExecuteError::SigintUnwind`) | A (row-11 ruling) | part of the run path's outcome surface (`Session::run` + `RunOutcome`): the host must be able to read how a run ended and service the engine's signal bookkeeping |
| snapshots & misc accessors (`aliases_snapshot`, `alias_expansion_enabled`) | C (case-by-case) | promote only if a later census shows a second consumer; otherwise internals. `bind_registry_snapshot` / `bind_registry_generation` ride the gated bind surface (§4's bind row), not this one |
| debugger front doors (`run_dap_stdio`, `run_debug_cli`, `DapLaunchPreset`, `DebugCliPreset`) | B | driver-layer front doors at the crate root — the design's §10 forms of the one debug session; they stay driver surface, not `Session` methods, and move with the driver layer at the split |
| `builtins::bind` spec helpers (`BindingSpec`, `BindSpecError`, the `parse_*` / `format_*` / `strip_*` helpers) | C | ride the gated bind surface with the `shell::bind_registry` row above: consumed only inside niubash's `bind_surface.rs` behind its `internals` feature |

## 5. The canary: how the freeze is enforced

Two halves, by design — a fast tripwire and an authoritative compile:

1. **Ledger tripwire** (`scripts/check-api-ledger.py` in rubash,
   stdlib-only): parses §6's symbol block and verifies every recorded
   symbol still exists in the rubash tree. Runs in rubash CI
   (`.github/workflows/niu-canary.yml`, job `ledger`) and can be run
   by hand: `python3 scripts/check-api-ledger.py`. It proves presence
   by declaration patterns only — a tripwire, not a type checker.
2. **Pairing compile** (same workflow, job `pairing`): checks out
   niubash master beside this tree (the sibling layout niubash's
   `[patch]` expects), and runs `cargo check --workspace --locked`
   in niubash against this exact rubash tree. A symbol that vanishes
   or changes signature fails the *engine's* CI, before merge — the
   failure mode whose absence let the lost-kill-hook incident reach
   the consumer's master.
3. **Same-source check** (same job): the niubash copy of this ledger
   must be byte-identical to this one (`cmp`), so "the ledger" can
   never quietly become two ledgers. niubash carries the mirror check
   in its own `.github/workflows/api-ledger-sync.yml`.

4. **Completeness check** (Ledger v3; same script, `--niu` mode):
   the tripwire also scans the paired niubash tree's `crates/` and
   `src/` for every `rubash::…` path token and verifies each one
   is recorded in §6 — a consumed path is covered when an entry
   names it exactly, or when it is a module prefix of a recorded
   entry (a module is covered by the symbols recorded under it).
   The pairing job runs it right after the same-source `cmp`, so
   a newly consumed symbol that nobody recorded fails the
   engine's CI the same way a vanished one does. Recorded-but-
   unconsumed `item`/`variant` entries only warn: the ledger
   legitimately carries staged exits (the `builtins::kill`
   replacements were recorded before the consumer finished
   moving). `method` entries are exempt from the warning —
   receiver-form calls are invisible to a path scan, so an
   unconsumed-looking method entry proves nothing either way.

Demonstration record (delete-a-symbol goes red, restore goes green):
`docs/api-ledger-canary-demo.md` in rubash.

## 6. Machine-checkable symbol block

`scripts/check-api-ledger.py` parses exactly this block. Entry kinds:
`item` (a `pub` item at the path), `variant` (an enum variant),
`method` (a `pub fn` of that name on the named type). Keep it sorted
by kind, then path; the census tables above are the human record of
the same set.

```api-ledger
item rubash::Ast
item rubash::DapLaunchPreset
item rubash::DebugCliPreset
item rubash::Executor
item rubash::Token
item rubash::TokenKind
item rubash::builtins::bind::BindSpecError
item rubash::builtins::bind::BindingSpec
item rubash::builtins::bind::format_invalid_keymap
item rubash::builtins::bind::parse_positional_binding
item rubash::builtins::bind::parse_unix_command_spec
item rubash::builtins::bind::strip_set_keyword
item rubash::decode_to_visible_text
item rubash::executor::ExecuteError
item rubash::executor::bytes_to_shell_text
item rubash::executor::read_process_stdin_bytes
item rubash::executor::Executor
item rubash::executor::shell_pwd_display_path
item rubash::history::HistoryProvider
item rubash::invocation::ShellInvocation
item rubash::invocation::materialize_command_file
item rubash::lexer::has_unclosed_input_syntax
item rubash::lexer::has_unclosed_input_syntax_posix
item rubash::lexer::tokenize
item rubash::lexer::tokenize_with_initial_posix
item rubash::parser::Ast
item rubash::parser::CommandNode
item rubash::parser::ParseLoopOptions
item rubash::parser::WordMetadata
item rubash::parser::ast_print::pretty_print_command
item rubash::parser::parse
item rubash::parser::parse_with_options
item rubash::run_dap_stdio
item rubash::run_debug_cli
item rubash::script_driver::InteractiveExpansion
item rubash::script_driver::consume_background_wrapper_handshake
item rubash::script_driver::inject_console_ctrl_c
item rubash::script_driver::stdin_script_errexit_enabled
item rubash::script_driver::READ_STDIN_MARKER
item rubash::script_driver::bytes_to_script_text
item rubash::script_driver::check_binary_file
item rubash::script_driver::finish_shell
item rubash::script_driver::heredoc_delimiter_unclosed_quote
item rubash::script_driver::pre_process_interactive_line
item rubash::script_driver::prepare_interactive_history
item rubash::script_driver::run_interactive_stdin
item rubash::script_driver::run_script_with_history_at_line_offset_tracked
item rubash::script_driver::run_source_with_line_offset
item rubash::script_driver::run_stdin_script
item rubash::script_driver::script_uses_aliases
item rubash::script_driver::script_uses_history
item rubash::shell::bind_registry::BindEntry
item rubash::shell::bind_registry::BindKind
item rubash::shell::bind_registry::KEYMAP_NAMES
item rubash::usage::LONG_OPTIONS
item rubash::usage::LONG_OPTIONS_WITH_VALUE
item rubash::usage::SHORT_OPTIONS
item rubash::usage::invocation_help_name
item rubash::usage::is_short_option_letter
item rubash::usage::show_shell_help
item rubash::usage::show_shell_version
variant rubash::TokenKind::Assignment
variant rubash::TokenKind::BraceExpand
variant rubash::TokenKind::CommandSubst
variant rubash::TokenKind::Word
variant rubash::builtins::bind::BindingSpec::Function
variant rubash::builtins::bind::BindingSpec::Macro
variant rubash::executor::ExecuteError::CommandNotFound
variant rubash::executor::ExecuteError::ExitCode
variant rubash::executor::ExecuteError::ExpansionFailure
variant rubash::executor::ExecuteError::FatalFunctionError
variant rubash::executor::ExecuteError::IoError
variant rubash::executor::ExecuteError::Return
variant rubash::executor::ExecuteError::SigintUnwind
variant rubash::script_driver::InteractiveExpansion::Discard
variant rubash::script_driver::InteractiveExpansion::Execute
variant rubash::shell::bind_registry::BindKind::Execute
variant rubash::shell::bind_registry::BindKind::Function
variant rubash::shell::bind_registry::BindKind::Macro
method rubash::executor::Executor::alias_expansion_enabled
method rubash::executor::Executor::aliases_snapshot
method rubash::executor::Executor::bind_registry_generation
method rubash::executor::Executor::bind_registry_snapshot
method rubash::executor::Executor::builtin_command_names
method rubash::executor::Executor::call_function_with_env
method rubash::executor::Executor::complete_line
method rubash::executor::Executor::env_vars_snapshot
method rubash::executor::Executor::execute_ast
method rubash::executor::Executor::execute_command
method rubash::executor::Executor::execute_prompt_command
method rubash::executor::Executor::expand_prompt_string_mut
method rubash::executor::Executor::export_env
method rubash::executor::Executor::find_script_on_path
method rubash::executor::Executor::functions_snapshot
method rubash::executor::Executor::get_env
method rubash::executor::Executor::has_compspec
method rubash::executor::Executor::has_function
method rubash::executor::Executor::inherit_process_stdin
method rubash::executor::Executor::last_exit_code
method rubash::executor::Executor::inject_signal
method rubash::executor::Executor::interrupt
method rubash::executor::Executor::last_command_inverted
method rubash::executor::Executor::mark_login_shell
method rubash::executor::Executor::new
method rubash::executor::Executor::resolve_shell_path
method rubash::executor::Executor::resolve_shell_path_from_env
method rubash::executor::Executor::resolve_shell_path_process_entries_from_env
method rubash::executor::Executor::run_exit_trap_with_status
method rubash::executor::Executor::set_builtin_disabled
method rubash::executor::Executor::set_compatible_shell_path
method rubash::executor::Executor::set_env
method rubash::executor::Executor::set_history_provider
method rubash::executor::Executor::set_last_exit_code
method rubash::executor::Executor::set_positional_params
method rubash::executor::Executor::set_shell_option
method rubash::executor::Executor::set_shell_root
method rubash::executor::Executor::set_shopt_option
method rubash::executor::Executor::set_winuxcmd_path
method rubash::executor::Executor::take_exit_jump_pending
method rubash::executor::Executor::take_signal_discard_pending
method rubash::executor::Executor::take_parse_error
method rubash::executor::Executor::unset_env
method rubash::history::SessionHistory::size_limit
method rubash::invocation::ShellInvocation::parse
method rubash::parser::WordMetadata::literal
```

*Ledger v3, 2026-10-28 (1.5.0 Session contract publication):
the Tier A contract is published in-tree at `rubash::api` (§4
status note); the debugger front doors and the `builtins::bind`
helper family are recorded (§2.1/§6) after the v2 census missed
them, and §5's completeness check now guards the census itself;
`script_driver::run_script_with_history` leaves the consumed
surface (unreferenced in niubash; the symbol stays `pub`).
Ledger v2, 2026-10-28 (PLAN row 11): §0 census basis
re-stated; the `builtins::kill` hooks exited the consumed
surface (replacements recorded in §4/§6 — the items' demotion
to `pub(crate)` follows once niubash's migration is on
master); `invocation` ruled Tier B; session-configuration and
driver-path-resolution families ruled A and B respectively;
the bind registry's gated-surface home landed on the niubash
side. Ledger v1, 2026-10-25. Update rule: any change to the consumed
surface (a symbol appears, vanishes, or moves tiers) updates this
file — both copies — in the same change that causes it, with the
census basis in §0 re-stated.*
