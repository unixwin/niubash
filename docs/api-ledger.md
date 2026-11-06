# API Ledger — the engine surface niubash consumes

> This file is checked into **both** repositories (rubash and niubash)
> at `docs/api-ledger.md`. The two copies are one document: they must
> stay byte-identical, and the pairing CI on both sides enforces it
> (see §5). Edit one copy, mirror the edit to the other in the same
> change, or the canary goes red.
>
> Ledger v4 (1.5.0 Session migration, 2026-10-29): niubash's host
> consumption has migrated onto `rubash::api` — §2.3 records the
> Session-side census, §2.2 is re-censused to the retained tier-C
> seam set, the exited `script_driver` faces leave §6, and the
> `builtins::kill` demotion follow-up has landed.
>
> Status: M-0 ledger for the 1.5.0 Session API RFC (rubash#595;
> in-repo design record: `docs/design/1.5.0-session-api.md` in rubash).
> This ledger is the mechanical form of that RFC's §3.2 consumer census
> and §9 step M-0. During the 1.5.0 workspace split, the surface listed
> here is **frozen** in the RFC's terms: no listed symbol may be
> removed, renamed, or re-signed without a ledger update in the same
> change and a tier assignment for whatever replaces it (§4).
>
> Addendum (dump-strings / pretty-print library entries, 2026-10-29,
> rubash#1977 / niubash#656): the -D dump face and the pretty-print
> loop moved into the library (`locale::collect_locale_strings` /
> `render_locale_string_dump`, `parser::ast_print::
> pretty_print_input`); niubash's host shadow implementations are
> deleted and both binaries consume the entries. Tier: the
> pretty-print entry rides the Tier B `parser` family row in §4; the
> dump face is recorded as a Tier B `locale` family (§2.1 row + §4
> row) — a driver-layer text pass over the public lexer, in the
> module that already owns locale.c's surface.
>
> Addendum (special_paths census backfill, 2026-10-29,
> rubash#2011 / niubash#677): the `special_paths` registry family
> niubash's completion and syntax-highlighting faces consume is
> recorded (§0 backfill basis, §2.1 row, §4 row, §6 entries)
> after §5's completeness check flagged it as
> consumed-but-unrecorded — the module went public in
> rubash#2011 and niubash#677 began consuming it, but no census
> ever registered the family.
>
> Addendum (Executor::new census backfill, 2026-10-29,
> niubash#694): the constructor path `rubash::Executor::new`
> niubash's invocation-input face consumes is recorded
> (§0 backfill basis, §2.1 row, §4 row, §6 entry) after §5's
> completeness check flagged it as consumed-but-unrecorded —
> niubash#694's dump / pretty-print fix (landed by niubash#695)
> borrows a bare `Executor` to run the engine's
> `find_script_on_path` for a slash-less script operand, and
> no census ever registered the constructor under its root
> re-export spelling.
>
> Addendum (external route provider seam, 2026-10-30,
> niubash#567): the route capability family — the
> `rubash::api` re-exports `ExternalRouteProvider`,
> `ExternalRouteRequest`, `ExternalRouteStdio`,
> `ExternalRouteHandle`, `ExternalRouteOutcome`, the builder
> face `SessionBuilder::external_route_provider`, the
> Executor setter `Executor::set_external_route_provider`,
> and the adapter-consumed `ExternalRouteHandle::raw_or_zero` —
> is recorded (§2.1 row, §2.3 row, §4 row, §6 entries). The
> seam is new on the engine side in the same change: the
> dispatcher consults a registered provider for a resolved
> foreground external whose spawn is fully prepared, and
> niubash's FFI provider (niubash#567; the provider landed
> in niubash#708) registers through the builder face. It is
> NOT the retired buffered host-handler shape: the request
> carries live OS stdio handles, the complete child
> environment, the session cwd, and a cancellation poll.
>
> Removal (2026-11-01, niubash#727): the seam's only consumer
> — niubash's FFI provider — was deleted with the FFI removal
> (niubash PR #794), and the engine seam was removed in the
> same wave. The route family never shipped in a formal
> release. Its §2.1 clause, §2.3 row, §4 row, and §6 entries
> are removed with it; this addendum stays as the record.
>
> Addendum (DEFAULT_FUNCTION_BINDINGS census backfill, 2026-10-30,
> niubash#701): the engine's default binding table
> `builtins::bind::DEFAULT_FUNCTION_BINDINGS` is recorded
> (§2 row, §6 entry) after §5's completeness check flagged it
> as consumed-but-unrecorded — niubash#701's bind compatibility
> surface (and the whole-table mirror test niubash#702 added
> against it) consume the table, but no census ever registered
> it. The completeness red this raised on every rubash PR's
> pairing job is what surfaced the gap.
>
> Addendum (bind_readline_variable record, 2026-10-30,
> rubash#2183 / niubash#704): the Executor read probe
> `bind_readline_variable` — niubash's REPL reads the
> `keyseq-timeout` readline variable through it (via the §4
> tier-C executor seam, alongside `bind_registry_snapshot` /
> `bind_registry_generation`) — is recorded (§2.2 row, §6
> entry). The method itself is new on the engine side in the
> same rubash change (rubash#2183): niubash#704 was written
> against it before it existed, and that PR's CI is the
> E0599 that surfaced the gap. The record is staged ahead
> of niubash#704's merge, like the kill-hook records before
> it.
>
> Addendum (inputrc reader census backfill, 2026-10-30,
> niubash#710): the engine's inputrc parser entry
> `builtins::bind::parse_inputrc_with_env` and its
> environment struct `builtins::bind::InputrcEnv` are
> recorded (§2 row, §6 entries) after §5's completeness
> check flagged them as consumed-but-unrecorded —
> niubash#710's config.toml removal made niubash's
> startup actually read `$INPUTRC` / `~/.inputrc`
> through the engine parser (niubash-runtime's config
> reader), and no census ever registered the pair. The
> completeness red this raised on every rubash PR's
> pairing job is what surfaced the gap.
>
> Addendum (invocation shape record, 2026-10-31,
> rubash#2084 / rubash#2130): the typed invocation-shape
> family — the `rubash::api` types `InvocationShape`
> (+ constructors `new` / `interactive` /
> `login_interactive`) and `RespawnSource` (+ variants
> `Fresh` / `EngineChild`), the builder face
> `SessionBuilder::invocation_shape`, the readback
> `Session::invocation_shape`, and the Executor state
> accessors `invocation_interactive` / `invocation_login`
> / `set_invocation_shape` — is recorded (§2.1 row, §4
> row, §6 entries). The family is new on the engine side
> in the same change (rubash#2324): the builder applies
> the declared shape to the executor's typed invocation
> state at build, and the migrated carrier reads — 38
> decision-point reads of the two read accessors across
> 15 executor files, which used to key off the
> `__RUBASH_INTERACTIVE` / `__RUBASH_LOGIN_SHELL`
> env-table entries — read through them. niubash
> consumes none of it yet (its Shell construction names
> no shape), so the record is staged ahead of the host's
> adoption, like the kill-hook records before it.
> (§5's completeness check stays green with no new
> warnings: the host already names the `rubash::api`
> module itself, which covers the module's entries
> under the check's module-prefix rule — the staged
> status is a fact of this record, not of the check.)
> The
> recorded methods are inherent faces, not trait
> methods, so no UFCS-token caveat applies to them.
> The same change's per-flag builder setter
> `SessionBuilder::interactive` is recorded in §6
> with them, pairing its twin
> `SessionBuilder::login_shell` (recorded since v4);
> the structured face supersedes both setters, which
> stay public inherent faces of the builder.
>
> Addendum (synthetic-link root face signature change, 2026-11-01,
> niubash#758): the special_paths registry's synthetic-link family
> grows the `/proc/<pid>/root` entry, and the family's resolution
> face changes signature in the same engine change:
> `special_paths::synthetic_link_target` and
> `proc_vfs::proc_link_target` now take the caller-fed
> payload-root forms (`proc_vfs::ProcSelfRoot`, built by
> `proc_vfs::self_root_forms`) beside the session-cwd forms —
> the root's target is session state (the payload root the path
> mapper serves for `/`), so it travels the same caller-fed
> channel as self/cwd. The family stays engine-consumed:
> niubash's completion and highlighting faces read the new entry
> through the already-recorded classification and enumeration
> faces, so no §2 census count moves and no §6 entry is added;
> the record is this addendum, in both copies, per the update
> rule.
>
> Addendum (bind / history_expand census backfill, 2026-11-01):
> the bind family's function table and plain-form formatters
> (`builtins::bind::READLINE_FUNCTIONS`,
> `builtins::bind::format_macro_plain`,
> `builtins::bind::format_variable_plain`), the
> history-expansion engine family
> (`history_expand::HistChars` / `HistCtx` /
> `HistEngineState` / `HistLookup` / `history_expand`,
> with the `HistChars::default` /
> `HistEngineState::default` constructors niubash calls),
> and the tilde expander
> (`expand::tilde::tilde::expand_word_prefix`) are
> recorded (§0 backfill basis, §2.1 rows, §4 rows, §6
> entries) after §5's completeness check flagged them as
> consumed-but-unrecorded — niubash's host readline
> commands (the niubash#765-#767 batch: the funmap,
> dump-macros / dump-variables, `history-expand-line`,
> `tilde-expand`) consume them through niubash-runtime's
> readline_commands and repl modules, but no census ever
> registered them. The completeness red this raised on
> every rubash PR's pairing job is what surfaced the gap.
>
> Addendum (GAP-3 bind view record, 2026-11-05,
> niubash#863 GAP-3): the tier-A bind view family — the
> `rubash::api` types `BindView` / `BindEntryView` /
> `BindKindView` (+ variants `Execute` / `Function` /
> `Macro`) / `BindRemovalView` and the read face
> `Session::bind_view` — is
> recorded (§2.1 row, §2.3 row, §4 row, §6 entries). The
> family is new on the engine side in the same change:
> `Session::bind_view` captures the key-binding registry
> (generation, editing mode, the readline variable table,
> every recorded `bind -r` removal tombstone, and every
> entry with its canonical bytes from the
> engine's own `keyseq_bytes`, its written form, kind,
> resolved keymap and GNU dump line) as one owned snapshot,
> superseding the host's tier-C registry reads
> (`bind_registry_snapshot` / `bind_registry_generation` /
> `bind_editing_mode` / `bind_readline_variable` through
> the §2.2 seam) and its handwritten keyseq re-decoders.
> niubash consumes none of it yet (its `bind_surface.rs`
> still converts the tier-C snapshot), so the record is
> staged ahead of the host's adoption, like the
> invocation-shape record before it; the tier-C bind rows
> retire when the host lane lands.
>
> Addendum (GAP-4 completion view record, 2026-11-05,
> niubash#863 GAP-4 + GAP-4b): the tier-A completion-state
> view — the `rubash::api` type `CompletionView` and the
> read faces `Session::completion_view` /
> `Session::has_function` — is recorded (§2.1 row, §2.3
> rows, §4 row, §6 entries). The family is new on the
> engine side in the same change: `Session::completion_view`
> captures, through `&self` and in one owned snapshot, the
> session cwd (`Executor::session_cwd`), the whole variable
> table in `Executor::env_vars_snapshot`'s own rendering —
> attribute pseudo-entries included — presented sorted by
> name, with a content fingerprint over that rendering, the
> function names (`Executor::functions_snapshot`) and the
> open fd numbers (`Executor::open_fds`); `Session::has_function`
> is the `&self` form of the executor's function probe, the
> only other contract form (`StateViews::has_function`)
> sitting behind `views()`'s `&mut self`. Together they
> supersede the host's tier-C completion reads (the
> `env_vars_snapshot` / `env_vars` / `functions_snapshot` /
> `open_fds` / `session_cwd` / `has_function` reads through
> the §2.2 seam) and the host-side snapshot struct it
> re-synchronized after every line. niubash consumes none
> of it yet, so the record is staged ahead of the host's
> adoption, like the GAP-3 record before it; the seam's
> snapshot/probe consumer set shrinks when the host lane
> lands. The host's `/tmp` answer
> (`executor().resolve_shell_path("/tmp")`) is deliberately
> NOT part of the view — the path resolver's face is
> GAP-5's object.
>
> Addendum (GAP-5 path-resolution record, 2026-11-05,
> niubash#863 GAP-5): the tier-A path-resolution faces —
> `Session::resolve_shell_path` (the `&self` instance
> form), `Session::resolve_shell_path_from_env` and
> `Session::resolve_shell_path_process_entries_from_env`
> (the associated forms), and
> `Session::shell_pwd_display_path` (the associated
> display form) — are recorded (§2.1 row, §2.3 rows,
> §4 rows, §6 entries). The family is new on the engine
> side in the same change: each face delegates to the
> one resolver in `executor::path` through the
> `Executor` method (or, for the display form, the
> module function) that already wraps it, so the
> contract and the engine cannot disagree about a
> spelling; the ruled path contract (bare `/` is the
> payload root, Option B, the drive spellings) does
> not move. Together they supersede the host's tier-C
> resolution reads (`resolve_shell_path` /
> `resolve_shell_path_from_env` /
> `resolve_shell_path_process_entries_from_env`
> through the §2.2 seam) and its direct call to the
> `rubash::executor::shell_pwd_display_path` module
> function. niubash consumes none of them yet, so the
> record is staged ahead of the host's adoption, like
> the GAP-4 record before it; the §2.2 resolution rows
> and the §2.1 `executor` row's `shell_pwd_display_path`
> count zero out when the host lane lands.
>
> Addendum (GAP-6 &self variable-read record, 2026-11-05,
> niubash#863 GAP-6): the tier-A `&self` variable read
> faces — `Session::environment_full` (the full-table
> snapshot) and `Session::variable_value` (the
> single-value probe) — are recorded (§2.1 row, §2.3
> rows, §4 row, §6 entries). The family is new on the
> engine side in the same change: `environment_full`
> returns the whole variable table in
> `Executor::env_vars_snapshot`'s own rendering —
> attribute pseudo-entries included — presented sorted
> by name through the one rendering
> (`completion_view::full_env_sorted`) it shares with
> `CompletionView`'s `env` component, so the face, the
> completion capture and the snapshot itself cannot
> disagree about the table; `variable_value` is the
> `&self` form of the executor's raw table read
> (`Executor::get_env`), deliberately NOT the `$name`
> expansion read — dynamic getters, nameref resolution
> and the typed-cell layer remain
> `VariablesView::resolved_value`'s semantics, and the
> GAP-6 consumer pins record both readings per shape.
> Together they supersede the host's remaining tier-C
> variable reads: the §2.2 full-table snapshots
> (`env_vars_snapshot` / `env_vars` — the contract's
> `environment()` is exported-only) and the read probes
> in `&self` host methods (the 8 lib-code `get_env`
> seam reads). niubash consumes neither yet, so the
> record is staged ahead of the host's adoption, like
> the GAP-5 record before it; the §2.2 rows zero out
> when the host lane lands.
>
> Addendum (GAP-7 completion-attempt record, 2026-11-05,
> niubash#863 GAP-7): the tier-A completion-attempt guard
> — `Session::begin_completion_attempt` opening a
> `CompletionAttempt` — is recorded (§2.1 row, §2.3 row,
> §4 row, §6 entries). The guard is the contract
> replacement for the host's thread-local shell lend
> (`COMPLETION_BRIDGE`): the host opens one guard per
> attempt from the `&mut Session` it holds, drives the
> attempt's execution faces through the guard
> (`complete_detailed`, `call_function_with_env` for the
> widget/compdef callbacks) and reads live state through
> the guard's `&Session` lend (`session()`). The scoped
> `&self` context alternative was rejected on evidence,
> not by fiat: an attempt EXECUTES — it runs compspec
> functions, captures `COMPREPLY`, mutates the
> governing-compspec copy through `compopt`, and lets
> function bodies write session state — and no `&self`
> context can own that without interior mutability
> across the whole executor; the read half already has
> `&self` faces. The prototype differential (three pins,
> green on master before the change) showed the engine
> side was already live under nesting: an inner `compgen`
> inside a -F function sees functions defined during
> the outer attempt, the bare-`compopt` window survives
> an inner compgen, and a nested dynamic resolution
> keeps the outer governing-compspec copy while the
> inner function's side effects persist. The dark faces
> were host-side only — the scoped bridge answering
> "no bridge" under nesting — so the guard's job is to
> carry that live property across the seam, not to
> create it. Alongside the guard, the completion window
> itself changed from a bool to a depth counter
> (`line_completion_active` → `line_completion_depth`,
> armed/disarmed by an RAII guard in
> `Executor::complete_line_detailed`): an inner attempt
> returning can no longer close the outer attempt's
> `compopt` window. True re-entrant
> `complete_detailed` calls are unreachable from
> either side today (the single-threaded host cannot
> borrow twice), so the depth is a hardening for the
> guard era, not a fix for a live bug; the pins prove
> the reachable nested semantics plus the guard's
> sequential-live behavior. The apply face for the
> post-build invocation object is NOT landed here:
> GAP-7 and GAP-12 share the `ShellInvocation` object
> as input but are different faces — GAP-7 is the
> apply face, GAP-12 the read-back faces — and an
> apply without a read face cannot be pinned, so the
> host's `apply_invocation_before_startup` stays
> tier-C until a follow-up pairs apply + read faces.
> Pairing closed (niubash#863 GAP-12): the follow-up has
> landed — `Session::apply_invocation_before_startup`
> plus the read face `Session::applied_invocation_shape`
> (with `Session::finish_invocation_startup`,
> `Session::remove_env` and `Session::mark_login_shell`
> beside them); see the GAP-12 addendum.
> niubash consumes the guard not yet, so the record is
> staged ahead of the host's adoption, like the GAP-6
> record before it; the §2.2 rows are untouched by
> this PR (the thread-local is host-side, and its
> retirement is the host lane's row to zero), and the
> §2.3 `begin_completion_attempt` count goes 0 → N
> when the host lane lands.
>
>
> Addendum (GAP-8 finish-guard record, 2026-11-05,
> niubash#863 GAP-8): the tier-A finish guard
> — `Session::begin_finish` opening a `Finish` — is
> recorded (§2.1 row, §2.3 rows, §4 rows, §6 entries).
> The guard is the contract replacement for the host's
> five hand-composed finish funnels and its tier-C
> trap-step seam reads (`run_exit_trap_with_status` and
> `close_all_persistent_output_process_substitutions`
> through `executor_mut`): the host opens one guard per
> finish from the `&mut Session` it holds, with the
> route's pending status as a value (the value the
> one-shot funnel used to inject through
> `set_last_status` before a consuming `close`), and
> drives the finish's coarse steps through it
> (`run_exit_trap`, `final_cwd`,
> `run_stream_end_duties` with the engine history step
> gated per route, `finish_all`), interleaving its own
> duties — the post-trap cwd record, the host-provider
> history flush, the process exit — between them. The
> session is NOT consumed, so the interactive route,
> whose session lives in a shared cell and whose
> history is the host provider's store, finishes
> through the same guard. `Session::close` /
> `close_with_report` are re-based on the guard in
> the same PR (signatures and `CloseReport`
> unchanged), and `close_with_report` ENTERS the
> ledger with this record: it was a pub tier-A-shaped
> face recorded nowhere — no §2.3 row, no §6 entry
> — the accretion shape this ledger exists to catch.
> The re-base also settles the finish order (captain
> adjudication D1): on the plain path the EXIT trap
> now runs BEFORE the history write, following GNU
> `exit_shell` (shell.c:1000-1002 the trap,
> shell.c:1007-1009 the save; `maybe_save_shell_history`,
> bashhist.c:488, reads HISTFILE/HISTFILESIZE at the
> write, so the trap's edits and additions govern the
> save). The pass as previously composed wrote history
> first — its steps were modeled on the termsig
> funnel — which diverged exactly when the pass itself
> was the first to run the trap (the close and
> end-of-stream routes; the `exit`-builtin routes had
> already consumed the trap at dispatch). The termsig
> sub-order (sig.c:590-667: history first, SIGHUP
> hangup before the trap, die-by-signal last) is
> preserved exactly, inside the same pass. The
> differential (probes A–E against the GNU oracle)
> and the order pin
> (`issue863_gap8_finish_order_history_write_observes_post_trap_state`)
> are recorded with the engine PR. niubash consumes
> the guard not yet, so the record is staged ahead of
> the host's adoption, like the GAP-7 record before
> it; the §2.2 trap-step rows zero out when the host
> lane lands, and the §2.3 `begin_finish` count goes
> 0 → N then.
>
> Addendum (GAP-9 token-view record, 2026-11-05,
> niubash#863 GAP-9): the tier-A token view
> — `Session::token_view` projecting the lexer onto
> caller-fed text as `TokenView` / `TokenSpan` /
> `TokenKindView` / `TokenRole` — and the completeness
> face `Session::needs_more_input` are recorded (§2.1
> row, §2.3 rows, §4 rows, §6 entries). The view is
> the contract replacement for the host editor's two
> private tokenizers (the highlighter's word scanner
> and the completer's shell-word splitter) and for its
> direct tier-B lexer reads: spans carry absolute byte
> offsets over the input as fed, dequoted values built
> from the raw source slices (the lexer's in-band
> carrier bytes never cross the surface), command
> positions and roles folded at projection time from
> the lexer's own stamps and the engine's builtin
> table, and comment spans synthesized from inter-token
> text (the lexer drops comments). Recorded difference
> from the GAP-3/4 views: the input is caller text, not
> session state; tokenization derives in the session's
> posix mode, exactly as `Session::parse` derives (the
> one tokenization delta between the modes is `time`,
> a reserved word only outside posix mode).
> `Session::needs_more_input` ENTERS the ledger with
> this record: the §6.5 completeness family has had
> this §4 target home since the RFC, and the face
> itself is as old as the Session, but it was recorded
> nowhere — no §2.3 row, no §6 entry — the accretion
> shape this ledger exists to catch. Its verdict
> parity with the host's scanner walk is pinned over
> the adjudicated subset of the host completeness
> corpus (the GAP-9 Q-B differential, 54 of 66 rows
> agreeing) in the engine's consumer tests; the two
> divergence classes — heredoc-pending (the face
> carries no heredoc-pending signal; the host's
> heredoc arm reads the lexer's unterminated-body
> stamp directly today) and the function-body stray
> close (`f() {\n)` and kin, terminal for both the
> host walk and the engine's reader, not for the
> face's token walk) — are recorded UNCLASSIFIED in
> the GAP-9 engine lane report pending the captain's
> classification, and the host lane does not adopt the
> face for those shapes until they are classified.
> One fidelity note the host lane inherits: a brace
> group is ONE folded lexer token, so the view does
> not project a group's interior words — the host's
> character scanner paints them today, and the host
> lane records how it keeps that painting. niubash
> consumes neither face yet, so the record is staged
> ahead of the host's adoption, like the GAP-8 record
> before it; the §2.1 `lexer` row's host counts retire
> (2 → 1: the heredoc-arm `tokenize` read) when the
> host lane lands.
> Round 2 (GAP-9 captain adjudications executed,
> 2026-11-05): both divergence classes are classified
> DIVERGE — engine reader governs — and the face is
> fixed in the same PR: heredoc-pending joins the
> completeness composition (the token walk reads the
> lexer's unterminated-body stamp; parse.y:3119
> `gather_here_documents` — an unread body is part of
> the still-open read), and the composition gains a
> terminal channel (a token-walk terminal verdict —
> stray `)`, foreign keyword closers, stray
> `;;`-family — overrides the text half's
> continuation verdicts; parse.y:6724 `yyerror` ends
> the read at the token), with the parked-feeder
> verdict fusions making the same cut so the fresh
> and parked paths agree. The Q-B differential now
> stands at 66 of 66 AGREE, pinned in the consumer
> tests. The brace-group fidelity note is discharged
> in the same round: the projection descends into
> folded brace groups (recursively), projecting the
> interior words with their own spans, positions and
> roles; command substitutions remain unprojected
> (word-shaped units, captain-ruled).
> Addendum (GAP-10 history-provider install record, 2026-11-05,
> niubash#863 GAP-10): the post-build history-provider
> install is a contract face — `Session::set_history_provider`,
> a pure lift of the executor setter (replace-always,
> unguarded; install-once remains the host's guard) —
> recorded (§2.3 row staged 0, §2.2 retained row retired
> 1 → 0, §4 rows, §6 entry). The ruling (captain
> adjudication Q-A → A2) closes retained-seam row 13, the
> last assembly-class member of the retained tier-C census:
> the recorded exception — the install reached through the
> seam, because a construction-time provider would exist on
> script routes too — is rewritten: builder injection
> exists (`SessionBuilder::history_provider`) and niubash's
> non-adoption of it stands with its route-discipline
> reason; the post-build install home is now the Session
> face. The same record carries the R-A5 semantics-division
> table (Q-C): the two history planes — the engine's
> `SessionHistory` on provider-free routes, the installed
> provider's store on interactive routes — divide the
> numbering base, stifle, timestamps, session rules,
> record filters, file format, the `-a`/`-w`/`-r`/`-n`
> faces, `last_line_added`, and HISTFILE retarget between
> them; the table is written in full in the
> `src/history.rs` module doc beside the trait and
> annotated on §4's history row, and its `history_base` /
> `last_line_added` rows rest on probes U-2/U-3 against
> GNU 5.3.20 (byte-identical listings: stifle numbering
> continuity under a small HISTSIZE, and both fc
> current-line arms). Q-D: `Session::history_entries` /
> `Session::set_history_entries` — tier-A faces recorded
> nowhere — ENTER the ledger with this record (§2.3 rows
> staged 0, §6 entries). Q-B: “one store” is ruled per
> plane (the host pair already converged under
> niubash#773/#774); list convergence is recorded as an
> input to Stage-2 project ③ scoping, not GAP-10 work.
> niubash consumes the new face not yet; the host lane
> adopts it, and the GAP-10 host migration completes the
> §2.2 retirement in substance.
> Addendum (GAP-11 candidate kind, 2026-11-05,
> niubash#863 GAP-11): each `Candidate` carries the
> engine-side kind its generator knew — `CandidateKind`
> (`Directory` / `Symlink` / `Executable` / `File`), a
> filesystem-only taxonomy mirroring the host's vocabulary, or
> `None` when the producer could not classify from evidence
> already in hand. Kind is marked at generation (the
> generator-classification principle: kind names the entry as
> the producing action classified it — a directory action
> follows links, so a symlink-to-directory it produces is
> `Directory`; the same entry in a plain file listing stays
> `Symlink`), rides the existing `complete_detailed` channel
> (§2.3 row, staged 0 — the host lane adopts it), and never
> leaks into text (`compgen` output is byte-identical, pinned).
> Dynamic candidates (`-F` COMPREPLY, `-C` output) stay
> unmarked; the attempt's filename flavor is carried instead
> by the `CompletionPolicy.filenames_intent` bit (a
> spec-intent fact, never a per-candidate claim). Marking uses
> only in-hand evidence — the dirent's class, the followed
> stat a directory action already performs, the PATH walk's
> executability verdict — and adds no per-candidate stat
> (the #747 budget): a regular file reads `File` even when
> executable (the host refines exec-ness from its memoized
> verdict), and an unclassifiable entry stays unmarked.
> Per-producer marking table: `-A directory` → Directory for
> every entry; `-A file` → Directory / File / Symlink from
> the dirent, unmarked for fifo/socket/device; `-A command` →
> Executable for PATH-walk entries, unmarked for aliases /
> keywords / functions / builtins; every other action arm,
> globpat matches and `-W` wordlists → unmarked; `-o
> dirnames` / `-o plusdirs` / `-o default` fallbacks and the
> no-spec filename fallback → marked through the same
> filename generator. Compatibility: `Candidate::new` /
> `described` default kind to `None` (constructor users are
> source-compatible); struct-literal construction of
> `Candidate` outside the tree breaks on the new field — a
> 1.5.1 contract change recorded here, with no
> `#[non_exhaustive]` added. The host lane deletes its own
> `CandidateKind` and uses this type (GAP-11 Q-E(ii)).
> Addendum (GAP-12 post-build invocation faces, 2026-11-05,
> niubash#863 GAP-12): the apply face GAP-7 deferred, paired
> with its read face. `Session::apply_invocation_before_startup`
> applies a parsed `ShellInvocation` to a live session before
> startup files run — the apply body plus the hop the host used
> to write by hand (when the invocation is interactive, the typed
> interactive flag is armed through `set_invocation_shape`; the
> apply body itself arms `expand_aliases` for interactive and
> `mark_login_shell` for login, but never the typed interactive
> flag) — and the Session retains the invocation, so
> `Session::finish_invocation_startup` can run the second phase
> (restrictions must not be live while startup files source, GNU
> shell.c:679-744) without the host holding a clone. The live
> read `Session::applied_invocation_shape` reads the executor's
> typed invocation state — interactive/login as applied,
> including `enter_interactive` and mark corrections — never the
> exported `__RUBASH_*` carriers (rubash#2129: a respawned child
> must not adopt a parent's login shape from an inherited
> carrier); `respawn` is the construction fact carried from the
> declared shape. `Session::remove_env` is the Session home of
> the restore-grade removal (a pure lift of `Executor::remove_env`:
> env-table removal, typed-flag clearing for the carriers, journal
> record, typed cell left as-is) — NOT the `unset` builtin's leaf,
> whose differential (readonly refusal, typed-cell drop, no
> journal, carrier leaves a shape lie) is pinned in the GAP-12
> consumer pins. `Session::mark_login_shell` is the login funnel
> as a companion face (carrier + `login_shell` shopt together);
> the route classification that decides to call it stays host
> knowledge. No journal read/drain face: the journal is pinned
> only as the removal face's implementation semantics (a removal
> is journaled; Drop restores the birth value), and no consumer
> exists for a read face. The five faces are recorded staged
> ahead of the GAP-12 host adoption (§2.3 rows, staged 0); the
> §2.2 `unset_env` row retires with this record — its recorded
> count was 1, the true pre-migration count is 3 (the host's
> construction carrier scrub plus the two restore arms; another
> drift instance for the GAP-13 recount wave) — and the
> `mark_login_shell` row retires 1 → 0 with it. The
> `Session::invocation_shape` readback is untouched: declared
> and live are now an explicit pinned distinction.
> Addendum (GAP-13 bookkeeping recount + unrecorded faces,
> 2026-11-06, rubash#2833): the ten-round re-audit's
> bookkeeping part. The §2.2 retained-seam table is
> re-counted against niubash master `413847b7` by receiver
> grep: claimed-live 21 → true live 10. Eleven rows are
> drift — migrations landed long ago and the rows were
> never zeroed: `set_env` 10 → 0, `export_env` 1 → 0,
> `set_shopt_option` 2 → 0, `take_exit_jump_pending` 2 → 0,
> `bind_registry_snapshot` 1 → 0, `bind_registry_generation`
> 2 → 0, `execute_command` 1 → 0, `last_command_inverted`
> 1 → 0, `has_function` 1 → 0, `has_compspec` 2 → 0,
> `call_function_with_env` 1 → 0. Three rows are corrected,
> not zeroed: `take_parse_error` 4 → 3, `alias_expansion_enabled`
> 4 → 1, `find_script_on_path` 1 → 2 (the second site is the
> bare-executor call in `main.rs`, beside the Session-door
> call). The drift mechanism is recorded in the §2.2
> recount paragraph: rows were written at migration time
> and never re-read, and the canary's completeness half
> scans `rubash::…` path tokens, which cannot see
> receiver-form method calls — so a stale method count
> fails no gate. The Session-method census (§5.5) closes
> that blind spot mechanically. The same wave corrects
> §2.1 (`CommandNode` 14 → 11, `pretty_print_command`
> 1 → 0, `run_script_with_history_at_line_offset_tracked`
> 1 → 0, `bytes_to_script_text` 1 → 3) and records four
> Session faces the re-audit found consumed but nowhere
> in the ledger (§2.3 rows + §6 entries):
> `Session::glob_filename`, `Session::notify_terminal_size`,
> `Session::expand_shell_words`, and `Session::environment`
> (the last caught by the census itself — the careful
> human audit missed it), plus the `Session::call_function_with_env`
> row the §2.2 retirement left row-less. The GAP-10 host
> adoption landed with niu #917 (merge `413847b7`), so the
> `Session::set_history_provider` §2.3 row reads 1 now.
> The two bind/readline assembly verbs (`Session::register_binding`,
> `Session::initialize_readline`, rubash#2833 Q-A/Q-B) are
> recorded NOWHERE by this wave, per the GAP-13 ruling
> Q-G — they enter the ledger with the entity engine PR.
> Addendum (GAP-13 bind/readline assembly faces, 2026-11-06,
> rubash#2833): the ten-round re-audit's entity part. The
> bind/readline ASSEMBLY is contract now — the two verbs the
> bookkeeping wave recorded nowhere (Q-G):
> `Session::register_binding` (the engine converts the
> `BindKindView` and constructs the `BindEntry` itself, into
> the same registry pool the `bind` builtin writes; keymap
> None resolves at registration, a register clears the
> `bind -r` tombstone, same-key replaces in place,
> generation +1 per call) and `Session::initialize_readline`
> (a pure lift of the executor's inputrc read; ordering —
> after rc, before the first snapshot — is the caller's duty,
> the face does not enforce it; idempotent by the engine
> flag; missing files silent), plus the membership probe
> `Session::is_known_keymap` (plain `KEYMAP_NAMES`
> membership, no alias folding — folding lives in the
> register path). The write path this retires: one host
> production site per verb — the bind surface's
> `register_bind_entry` call (`bind_surface.rs:46`) and the
> shell's `load_engine_bindings` call (`shell.rs:3145`) —
> against two engine-internal sites each (the `bind`
> builtin's two `register_bind_entry` arms; its lazy init
> plus the interactive-stdin driver's entry init). The three
> faces are recorded staged ahead of the GAP-13 host adoption
> (§2.3 rows, staged 0); that lane deletes `bind_surface.rs`
> entirely and the `BindEntry` / `BindKind` §6 items revert
> to internal with it (Q-E — see the §2.1 note). The same
> wave records `Session::parse` in §6 as a staged entry
> (unconsumed; its §4 target home predates it) and adds the
> GAP-12 follow-up read `Session::applied_invocation_shopt_flag`
> (rubash#2848): the retained invocation's explicit `-O`/`+O`
> flag for a name (last entry wins, `None` when the
> invocation said nothing) — the read `enter_interactive`
> needs for `expand_aliases`, which the shape read cannot
> carry; the apply face's doc now says the host's
> `startup_invocation` field deletes only once the host
> adopts the retained-invocation reads. §6 grows 279 → 284.
> Addendum (GAP-13 ledger cleanup, 2026-11-06,
> rubash#2833): the host lane landed (niu #921, merge
> `9388c9cf`) — the §6 `BindEntry` / `BindKind` /
> `KEYMAP_NAMES` items plus the three `BindKind` variants
> leave the ledger (niu production refs to the
> bind_registry types are 0), and the three §2.3 faces
> (`register_binding` / `initialize_readline` /
> `is_known_keymap`) read 1 each at production caliber.
> §6 shrinks 284 → 278. The `BindEntry` / `BindKind`
> internal demotion in the engine has not landed (the
> symbols stay `pub`); it is a separate engine follow-up,
> not a precondition for this record.
> Addendum (Q-E demotion lands, 2026-11-06,
> rubash#2853): the engine follow-up the cleanup addendum
> left open has landed — `pub mod bind_registry` /
> `BindEntry` / `BindKind` / `KEYMAP_NAMES` are `pub(crate)`
> now (the `shell` module itself stays `pub`; only its
> `bind_registry` child demotes, since `shell` still exports
> `ShellState` / `Variable` / `VariableStore` and friends).
> `Executor::bind_registry_snapshot` demotes with them (its
> public signature leaks the demoted `BindEntry`; E0446
> forces the demotion), so its §6 item leaves the ledger:
> §6 shrinks 278 → 277. The §2.2 `bind_registry_snapshot`
> row stays at 0 — permanently now, not drift — and the §4
> snapshots row no longer lists it as riding the gated bind
> surface. `bind_registry_generation` stays `pub` and keeps
> its §6 item; `BindEntryView` / `BindKindView` stay `pub`
> as the public view types.
> Addendum (B5-11, 2026-11-06,
> rubash#2856): `Session::has_compspec_at` / `Executor::has_compspec_at`
> enter the ledger as the position-aware compspec gate (the
> command-name form `has_compspec` cannot name the `-E` empty line
> or the `-I` initial word), recorded §6 + §2.3 (staged 0 ahead of
> the host adoption) + §4 completion row; `has_compspec` stays. The
> same PR fixes the engine's own blank-line corners the gate
> exposes: W1 (a whitespace-only line at end-of-line takes
> `-I`/default, never `-E`), W2 (column 0 ahead of the command
> name takes `-E`), W3 (an empty line with only `-I` registered
> resolves to `-I` via the bashline.c post-chain arm) — all pinned
> at the `complete_line` level (b01-b18 + arm-8 shapes) with the
> gate asserted alongside. §6 grows 277 → 279 (two items: the
> `Session` face and the `Executor` face).
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

Re-census for Ledger v4 (Session migration, 2026-10-29), same rules:

- rubash companion branch base `5b63c803` plus the tier-C seam
  change (the `Session::executor` / `executor_mut` door, §4)
- niubash migration branch (post-#664 master `c6bb8ae8` plus the
  migration commits)

The migration moves the bulk of the §2.2 method traffic onto the
Session faces (§2.3): of the methods the v3 table lists, 14 leave
the called set entirely (the builder and the views carry them
now), 27 remain, and 4 seam calls the earlier censuses missed
(`env_vars`, `run_bind_x_command`, `script_fd0_line`,
`close_all_persistent_output_process_substitutions`) are
recorded for the first time — a retained seam set of 31. The
`script_driver` run/stdin/interactive-prep faces and
`finish_shell` leave the consumed surface the same way
`run_script_with_history` did in v3.

Backfill census for the `special_paths` family (2026-10-29),
same rules:

- rubash master `318d686a92765cead96ffed29e69e6badfca5b9e`
- niubash master `9c21e7fd06bbd4b5509b104533cbed5af9ca5117`

The v4 census missed this family (see the header addendum).
Occurrences by the rule below: `classify_special_path` (2),
`special_dev_entries` (4), `synthetic_proc_dir_entries` (3),
and `SpecialPathClass::Filesystem` (2), spread over exactly
**2 files**: `crates/niubash-runtime/src/completion/path.rs`
and `crates/niubash-runtime/src/syntax_highlighting.rs`.

Backfill census for the `Executor::new` constructor path
(2026-10-29), same rules:

- rubash master `b41150e41916433ac4bb10b1483f3b4640171359`
- niubash master `4f9e8a1715d71dfb9c4aa29a04a55e92164c4609`

The v4 census missed this path (see the header addendum).
Occurrences by the rule below: `rubash::Executor::new` (1),
in exactly **1 file**: `src/main.rs` — the bare executor
niubash#694's invocation-input operand reader
(`read_invocation_script_operand`) constructs to borrow
`find_script_on_path` (§2.2) for a slash-less name. It is
the one direct `Executor` construction left in the host:
every `Shell` is built through the Session builder (§2.3)
and every other Executor arrives through the §2.2 seam.

Backfill census for the bind / history_expand families
(2026-11-01), same rules:

- rubash master `ee954e9c35b44fbd475d36d601826992fc563e14`
- niubash master `288b6b5250eab36fef2607eaa45a787079c6812e`

The v4 census missed these families (see the header
addendum). Occurrences by the rule below:
`builtins::bind::READLINE_FUNCTIONS` (1),
`builtins::bind::format_macro_plain` (1),
`builtins::bind::format_variable_plain` (1),
`expand::tilde::tilde::expand_word_prefix` (1),
`history_expand::HistChars::default` (1),
`history_expand::HistCtx` (1),
`history_expand::HistEngineState::default` (1),
`history_expand::HistLookup` (1), and
`history_expand::history_expand` (1), spread over exactly
**2 files**: `crates/niubash-runtime/src/readline_commands.rs`
and `crates/niubash-runtime/src/repl.rs`. The two
`::default` tokens are recorded under their types (§6
`item` entries for `HistChars` / `HistEngineState`): a
`Default` constructor is a trait impl, not a `pub fn`,
so the §6 `method` kind cannot represent it — the type
entry is its record, as the §2.1 row states.

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
| `api` | `Session` (+ §2.3 methods), `SessionBuilder` (+ §2.3 methods), `RunOptions`, `RunOutcome`, `Flow` (+ variant `ExitRequested`), `PreparedLine` (+ variants `Execute`, `Discard`) — the Tier A contract surface niubash's host drives since the Ledger v4 migration; plus the invocation-shape family `InvocationShape` (+ constructors `new` / `interactive` / `login_interactive`) / `RespawnSource` (+ variants `Fresh`, `EngineChild`), with the builder face `SessionBuilder::invocation_shape` and the readback `Session::invocation_shape` — the rubash#2130 construction-time shape declaration (recorded staged ahead of host adoption; see the header addendum); GAP-12 (niubash#863) pairs the declaration with the post-build apply/read faces (`Session::apply_invocation_before_startup` / `finish_invocation_startup` / `applied_invocation_shape`, with `Session::remove_env` / `mark_login_shell` beside them — §2.3 rows, staged 0; see the header addendum); plus the bind view family `BindView` / `BindEntryView` / `BindKindView` (+ variants `Execute` / `Function` / `Macro`) / `BindRemovalView`, with the read face `Session::bind_view` — the niubash#863 GAP-3 registry snapshot (recorded staged ahead of host adoption; see the header addendum); plus the completion view family `CompletionView`, with the read faces `Session::completion_view` / `Session::has_function` — the niubash#863 GAP-4 completer-state snapshot and `&self` function probe (recorded staged ahead of host adoption; see the header addendum); plus the path-resolution faces `Session::resolve_shell_path` / `Session::resolve_shell_path_from_env` / `Session::resolve_shell_path_process_entries_from_env` / `Session::shell_pwd_display_path` — the niubash#863 GAP-5 same-source tier-A forms of the `executor::path` resolver (recorded staged ahead of host adoption; see the header addendum); plus the `&self` variable read faces `Session::environment_full` / `Session::variable_value` — the niubash#863 GAP-6 full-table snapshot and single-value stored-value probe (recorded staged ahead of host adoption; see the header addendum); plus the completion-attempt guard family `CompletionAttempt` (+ methods `complete` / `complete_detailed` / `call_function_with_env` / `session`), with the opener `Session::begin_completion_attempt` — the niubash#863 GAP-7 reentrancy-aware attempt object replacing the host's thread-local shell lend (recorded staged ahead of host adoption; see the header addendum); plus the finish guard family `Finish` (+ methods `run_exit_trap` / `final_cwd` / `run_stream_end_duties` / `finish_all`) / `FinishOutcome` / `FinishDuties` / `SessionHistoryWrite`, with the opener `Session::begin_finish` and the report face `Session::close_with_report` — the niubash#863 GAP-8 step-decomposed finish object replacing the host's hand-composed finish funnels (recorded staged ahead of host adoption; see the header addendum); plus the token view family `TokenView` (+ methods `word_at` / `segment_words_before` / `last_separator_before`) / `TokenSpan` / `TokenKindView` / `TokenRole`, with the read face `Session::token_view` and the completeness face `Session::needs_more_input` — the niubash#863 GAP-9 lexer projection onto caller-fed text and the session completeness query (recorded staged ahead of host adoption; see the header addendum) |
| `executor` | `Executor` (type; +§2.2 methods), `ExecuteError` (2) + variants `ExitCode` (6), `Return` (4), `CommandNotFound` (3), `ExpansionFailure` (3), `FatalFunctionError` (3), `IoError` (1); `shell_pwd_display_path` (0) |
| root re-exports | `Executor` (1) + constructor path `Executor::new` (1; recorded in the 2026-10-29 backfill, see §0), `Token` (1), `TokenKind` (1) + variants `Word`, `Assignment`, `BraceExpand`, `CommandSubst` (1 each), `Ast` (use-list), `decode_to_visible_text` (1) |
| debugger front doors | `run_dap_stdio`, `run_debug_cli`, `DapLaunchPreset`, `DebugCliPreset` — root re-exports consumed by niubash's `--dap` / `--debug` entry points (missed by the v2 census; recorded in v3) |
| `builtins::bind` | `BindingSpec` + variants `Function`, `Macro`, `BindSpecError`, `format_invalid_keymap`, `format_macro_plain`, `format_variable_plain`, `parse_positional_binding`, `parse_unix_command_spec`, `strip_set_keyword`, `DEFAULT_FUNCTION_BINDINGS`, `READLINE_FUNCTIONS`, `InputrcEnv`, `parse_inputrc_with_env` — the bind builtin's spec-parsing helpers plus the default binding table niubash's bind surface mirrors, consumed inside niubash's gated bind surface (helpers missed by the v2 census, recorded in v3; `DEFAULT_FUNCTION_BINDINGS` recorded in the 2026-10-30 backfill, see header addendum). The `InputrcEnv` / `parse_inputrc_with_env` pair (recorded in the 2026-10-30 inputrc backfill, see header addendum) is the exception to the consumption site: niubash's startup inputrc reader (niubash-runtime's config module) calls it directly, not through the gated bind surface. The `READLINE_FUNCTIONS` table and the `format_macro_plain` / `format_variable_plain` plain-form formatters (recorded in the 2026-11-01 backfill, see header addendum) are a further exception: niubash's host readline layer calls them directly — the funmap in niubash-runtime's readline_commands module reads the table, and the repl's dump-macros / dump-variables faces format through the pair, over data the gated bind surface loads |
| `history` | `HistoryProvider` (7) — trait impl + `SharedHistoryProvider` injection; the one true capability trait today; plus the `SessionHistory::size_limit` associated helper (recorded in v3) |
| `history_expand` | `HistChars::default` (1), `HistCtx` (1), `HistEngineState::default` (1), `HistLookup` (1), `history_expand` (1) — the history-expansion engine niubash's host readline layer drives directly for its `history-expand-line` face and the first stage of `shell_expand_line`: niubash-runtime's readline_commands module implements `HistLookup` over the session entries, builds the `HistChars` / `HistCtx` context from the session's `histchars` value and posix mode, and calls `history_expand` over a fresh `HistEngineState` (recorded in the 2026-11-01 backfill; see §0 — the `::default` tokens are recorded under their types in §6) |
| `expand::tilde` | `tilde::expand_word_prefix` (1) — the tilde-prefix expander niubash's host `tilde-expand` readline command applies to the word at point over the session environment; consumed in niubash-runtime's repl module under the defining-module spelling `expand::tilde::tilde::expand_word_prefix`, which is the spelling §6 records (recorded in the 2026-11-01 backfill; see §0) |
| `script_driver` | `run_source_with_line_offset` (1; the unclosed-arm call left the consumed surface with GAP-2, niubash#898 — the stray-close arm's call remains), `run_script_with_history_at_line_offset_classified`, `script_uses_history`, `script_uses_aliases`, `check_binary_file`, `heredoc_delimiter_unclosed_quote`, `READ_STDIN_MARKER` (1 each unless noted); `run_script_with_history_at_line_offset_tracked` (0) and `bytes_to_script_text` (3) — the GAP-13 recount wave (rubash#2833, niubash master `413847b7`): the tracked arm left the consumed surface with the GAP-2 retirement, and the text conversion is called at three `main.rs` sites, not one. (Ledger v4: `run_stdin_script`, `run_interactive_stdin`, `prepare_interactive_history`, `pre_process_interactive_line` + `InteractiveExpansion`, and `finish_shell` have exited the consumed surface — `Session::run_stdin` / `run_interactive_stdin` / `prepare_line` / `close` carry that traffic now; the symbols stay `pub` in the engine.) |
| `parser` | `CommandNode` (11), `parse` (1), `parse_with_options` (1), `ParseLoopOptions` (1), `WordMetadata` (+ `literal`, §2.2), `ast_print::pretty_print_command` (0), `ast_print::pretty_print_input` (1); `Ast` is also consumed under this defining-module spelling (the root re-export above is the other spelling; both recorded in §6). (GAP-13 recount wave, rubash#2833: `CommandNode` 14 → 11 and `pretty_print_command` 1 → 0 — no niubash reference to the pretty-printer remains; the 11 `CommandNode` tokens are all `rubash::parser::CommandNode` type mentions in `shell.rs`.) |
| `lexer` | `tokenize` (1), `tokenize_with_initial_posix` (0), `has_unclosed_input_syntax` (0), `has_unclosed_input_syntax_posix` (1) — the two zeros are a correction carried with the GAP-9 ledger mirror (captain adjudication): a repo-wide grep of niubash master (f54af426, target/ excluded) finds no reference to `tokenize_with_initial_posix` or to the non-posix `has_unclosed_input_syntax` outside this ledger, so both stale (1)s are corrected to the measured figure; the posix variant's (1) stands — it is a live call in niubash-runtime's shell module |
| `locale` | `collect_locale_strings` (1), `render_locale_string_dump` (1) — the -D dump face (rubash#1977); `decode_to_visible_text` is recorded under root re-exports above |
| `special_paths` | `classify_special_path` (2), `special_dev_entries` (4), `synthetic_proc_dir_entries` (3), `SpecialPathClass` + variant `Filesystem` (2) — the special-path registry the engine's open points judge by, read by niubash's completion and syntax-highlighting faces (recorded in the 2026-10-29 backfill; see §0) |
| `shell::bind_registry` | **exited with the GAP-13 host lane (niu #921, merge `9388c9cf`)**: `BindEntry` (10), `BindKind` + variants `Execute` (6), `Function` (3), `Macro` (2), and `KEYMAP_NAMES` are no longer consumed — niubash's bind surface (`bind_surface.rs`) is deleted, so host production refs to the bind_registry types are 0 outside tests (the v2 re-census count was 23, all inside that surface). The registry's contract home is now the Tier A bind view (`Session::bind_view`, GAP-3) plus the assembly faces (`Session::register_binding` / `initialize_readline` / `is_known_keymap`, §2.3 rows, GAP-13). The symbols stay `pub` in the engine (the Q-E internal demotion has not landed — see the GAP-13 ledger-cleanup REPORT); the §6 items leave with this record. |
| `invocation` | `ShellInvocation` (3, + `parse`, §2.2), `materialize_command_file` (1) |
| `usage` | `LONG_OPTIONS` (3), `SHORT_OPTIONS` (2), `LONG_OPTIONS_WITH_VALUE` (1), `show_shell_version` (1), `show_shell_help` (1), `is_short_option_letter` (1), `invocation_help_name` (1), plus 2 module-level references |
| `builtins::kill` | **exited in row 11 (M-3)**: `note_console_ctrl_c` (2) and `consume_background_wrapper_handshake` (1) are no longer consumed — niubash calls the Tier B driver faces `script_driver::inject_console_ctrl_c` / `script_driver::consume_background_wrapper_handshake` instead, and the Tier A replacements are `Executor::inject_signal` / `Executor::interrupt`. The kill items' demotion to `pub(crate)` landed with the migration's companion change (Ledger v4); niubash consumes neither hook. (One of these hooks disappearing is what broke niubash master in the bulk-merge incident — which is why the exit is staged, not abrupt.) |

### 2.2 `Executor` methods niubash calls (receiver-verified)

Ledger v4: this table is the **retained tier-C seam set** — the
Executor methods niubash still calls directly after the Session
migration, reached through the `Session::executor` /
`executor_mut` door (§4) or through `&mut Executor` helper
parameters the host passes it to. Everything else the host used
to call on the Executor moved to the Session faces (§2.3). 32
distinct methods, occurrence counts by the §0 rule at the v4
census basis (the pre-migration table listed 41 entries; the
RFC §3.2 top set it cites is pre-migration history).

| Method | Calls | Method | Calls |
| --- | --- | --- | --- |
| `get_env` | 0 | `take_exit_jump_pending` | 0 |
| `set_env` | 0 | `call_function_with_env` | 0 |
| `resolve_shell_path` | 0 | `env_vars_snapshot` | 0 |
| `alias_expansion_enabled` | 1 | `set_shopt_option` | 0 |
| `take_parse_error` | 3 | `has_compspec` | 0 |
| `execute_ast` | 4 | `bind_registry_snapshot` | 0 |
| `resolve_shell_path_from_env` (assoc) | 0 | `set_history_provider` | 0 |
| `bind_registry_generation` | 0 | `run_bind_x_command` | 1 |
| `resolve_shell_path_process_entries_from_env` (assoc) | 0 | `has_function` | 0 |
| `take_signal_discard_pending` | 2 | `env_vars` | 0 |
| `run_exit_trap_with_status` | 0 | `close_all_persistent_output_process_substitutions` | 0 |
| `execute_command` | 0 | `unset_env` | 0 |
| `export_env` | 0 | `last_exit_code` | 1 |
| `builtin_command_names` (assoc) | 1 | `find_script_on_path` | 2 |
| `mark_login_shell` | 0 | `script_fd0_line` | 1 |
| `last_command_inverted` | 0 | `bind_readline_variable` | 1 |

Two further non-`Executor` methods are consumed:
`ShellInvocation::parse` (2) and `WordMetadata::literal` (1).

Retirement landed (niubash#863 GAP-5): the three resolution
rows above — `resolve_shell_path`,
`resolve_shell_path_from_env` (assoc) and
`resolve_shell_path_process_entries_from_env` (assoc) —
and the §2.1 `executor` row's `shell_pwd_display_path`
were the tier-C forms of the path resolver whose tier-A
`Session` faces are recorded in §2.3. The host migration
moved every resolution read to the Session faces, so
these counts now read 0; the methods themselves stay
`pub` in the engine.

Retirement landed (niubash#863 GAP-6): the two read classes
the note below names — the full-table snapshots
(`env_vars`, `env_vars_snapshot`) and the `&self` probes
(`get_env`) — are the tier-C forms of the variable reads
whose tier-A `Session` faces are recorded in §2.3. The host
migration moved every variable read to the Session faces, so
these counts now read 0; the methods themselves stay `pub`
in the engine. Survey correction, recorded here so the
engine-time figure in the header addendum stays intelligible
as history: the lane found 7 lib-code `get_env` probes, not
8 — the brief's "+4 more" overcounted by one (the tree holds
`THIS_SH`, `__RUBASH_POSIX_MODE`, the env-file expansion
probe, `PS1`, `NIU_TITLE`, and `PWD` twice); the addendum's
8 is the engine-time survey figure, the 7 here is the
landed count.

Retirement landed (niubash#863 GAP-7): the completion bridge
— the thread-local shell lend (`COMPLETION_BRIDGE`, the scoped
raw-pointer lend `SCOPED_COMPLETION_BRIDGE` with its
reentrancy flag, and the `install_completion_bridge` /
`with_completion_bridge` / `uninstall_completion_bridge`
functions) — is deleted, and its install point and seven
production reads route through the tier-A attempt guard
recorded in §2.3 (`Session::begin_completion_attempt`). The
reads: the completer's capture (the `CompletionView` through
the guard's `session()`), the compspec gate, the glob face,
the generation call (`complete_detailed` on the guard), the
compdef registry and call (`call_function_with_env` on the
guard — the §2.2 `call_function_with_env` row drops to the
widget leg alone), and the highlighter's known-command view
read (one short attempt). Drivers holding `&mut Shell` (the
probe surface) open the guard directly; the editor's
completer and highlighter hold the REPL's `Rc<RefCell<Shell>>`
behind `EditorShellHandle` — the `Send` settlement recorded
on that type — and open one guard per call through the same
drivers. Nested completion is live at every depth: the GAP-4
reentry pin's dark shape is inverted by the GAP-7 pins, and
the scoped lend's refusal semantics have no successor.
Survey correction, recorded here so the engine-time figure
in the header addendum stays intelligible as history: the
lane found TWO install points, not one — beside the editor
build in `repl.rs`, the CLI verb (`niu --completion-probe`)
installed the Rc bridge by hand in `src/main.rs`; it now
calls the library probe surface, which carries the whole
contract itself (niubash#831).

Retirement landed (niubash#863 GAP-8): the host's five
hand-composed finish compositions are deleted, and every
finish route drives the tier-A finish guard recorded in
§2.3 (`Session::begin_finish`). The five: (1) the tier-C
trap funnel `Shell::finish_with_exit_trap` (zshexit hooks +
`run_exit_trap_with_status` through the executor seam + a
direct persistent-output procsub close — the two §2.2 rows
above read 0 now); (2) the one-shot funnel's hand
composition (trap funnel, then the post-trap directory
parked in a `finish_cwd` field, the resolved status
injected through `set_last_status`, and a consuming
`Session::close` for the remaining duties — the field, the
injection, and the consumption are all deleted; the funnel
is the guard's one-call pass, and the §2.3 `Session::close`
row reads 0: the host no longer consumes a session to
finish it, and the `--cwd-state` recorder reads the
post-trap directory from the still-live session's `PWD`,
the #668 value the guard's outcome reports as
`final_cwd`); (3) the REPL session end, which composed
(1) by hand and never closed — it now drives the guard's
trap step, interleaves the host duties (the #646
trap-then-write provider flush), and runs the guard's
duties step with the engine history write gated off, so
the #642 double-write is impossible by construction;
(4) the startup-exit composition in the binary, re-pointed
at the same trap step + gated duties; (5) the three REPL
tails that ran the trap with a hard-coded 0/1 and
discarded the result (`let _ =`) — the Ctrl-D/EOF arm,
the line-editor error arm, and the no-editing EOF tail
now route through the session-end shape with the route's
real pending status (EOF: the last command's status, GNU
`exit_shell(last_command_exit_value)`), and the trap
step's resolved status decides the process code (captain
adjudication D2, a deliberate behavior correction). The
piped-stdin tail's second finish is also gone: the engine
driver already runs the full finish pass at stream end,
so that call could only re-report a consumed trap — the
host-only zshexit hooks, which no driver can run, stay as
the route's only host step. The finish order itself is
the engine pass's (captain adjudication D1: on the plain
path the trap runs before the history write, GNU
shell.c:1000-1009), which retires the split composition's
recorded ordering delta. The retained-set note below
names "the exit-trap funnel the REPL finish composes by
hand" — that entry is the funnel retired here.

Retirement recorded (niubash#863 GAP-10): the post-build
history-provider install leaves the retained set. The
install is a contract face now — `Session::set_history_provider`,
a pure lift of the executor setter (replace-always,
unguarded; the host's install-once guard remains the
discipline of record, host-side) — so retained-seam
row 13 closes and the row above reads 0 with this record;
the host lane's re-point of its one call site (interactive
entry, through the face instead of `executor_mut()`)
completes the retirement in substance. Builder injection
(`SessionBuilder::history_provider`) exists and stands;
niubash's non-adoption of it is recorded in §2.3 with its
route-discipline reason. The retained-set note below
names “the post-build history-provider install” — that
entry is the install retired here.

Retirement landed (niubash#863 GAP-12): the post-build
invocation faces leave the retained set. The apply face is a
contract face now — `Session::apply_invocation_before_startup`
plus the read face `Session::applied_invocation_shape` and the
second phase `Session::finish_invocation_startup` — so the
apply-path `set_invocation_shape` seam call (one site, the
host's hop-2 arm at `shell.rs:1703`, which never carried its
own §2.2 row: a method-level census gap of the kind the
ten-round re-audit documented) retires with it, and the
`unset_env` / `mark_login_shell` rows above read 0 with this
record. The removal half is a contract face too —
`Session::remove_env`, a pure lift of `Executor::remove_env` —
and the login-marking half rides with it as
`Session::mark_login_shell`. Survey correction, recorded here
so the engine-time figure in the header addendum stays
intelligible as history: the lane found THREE production
`unset_env` sites, not one — beside the restore arm
(`shell.rs:4701`), the host's construction carrier scrub
calls it twice (`shell.rs:776` for `__RUBASH_LOGIN_SHELL`,
`:778` for `__RUBASH_INTERACTIVE`); the recorded 1
undercounts the set by two, so the row's retirement is 1 → 0
in the ledger and 3 → 0 in substance (verified by receiver
grep on niubash master, 5ef4c48 — another drift instance for
the GAP-13 recount wave to cite). The `mark_login_shell` row's
recorded 1 was the true count (one site, `shell.rs:1730`).
The `Session::invocation_shape` readback is untouched:
declared and live are now an explicit pinned distinction
(see the GAP-12 addendum).

Recount (rubash#2833 GAP-13, the ten-round re-audit's
bookkeeping wave): the table above is re-counted against
niubash master `413847b7` by receiver grep
(`.<name>(` on the held executor, plus
`Executor::<name>` associated calls, production caliber —
test-module reads are noted in prose, not counted), and
the claimed-live 21 rows shrink to a true live 10:
`execute_ast` (4), `take_parse_error` (3, corrected from
4), `take_signal_discard_pending` (2),
`alias_expansion_enabled` (1, corrected from 4),
`run_bind_x_command` (1), `bind_readline_variable` (1),
`script_fd0_line` (1), `last_exit_code` (1),
`builtin_command_names` (assoc, 1), `find_script_on_path`
(2, corrected from 1 — the second site is the bare-executor
call in `main.rs:1164`, beside the Session-door call in
`main.rs:465`). Eleven rows are drift, not retirement:
their migrations landed in earlier waves and the rows
were never zeroed — `set_env` 10 → 0, `export_env` 1 → 0,
`set_shopt_option` 2 → 0, `take_exit_jump_pending` 2 → 0,
`bind_registry_snapshot` 1 → 0, `bind_registry_generation`
2 → 0, `execute_command` 1 → 0, `last_command_inverted`
1 → 0, `has_function` 1 → 0, `has_compspec` 2 → 0,
`call_function_with_env` 1 → 0 (its remaining leg is the
`Session` face, §2.3). The drift mechanism, recorded so
no later wave repeats it: a row's count was written at
migration time and never re-read against the live tree,
and the canary's completeness half (§5.4) scans
`rubash::…` path tokens, which cannot see receiver-form
method calls — a stale method count fails no gate. The
Session-method census (§5.5) closes that blind spot
mechanically; until it exists, a method row's count is
true only on the day it is grep-verified. The staged
zeros are NOT drift and are left alone: `unset_env`
(three live seam calls, GAP-12 host lane in flight) and
`mark_login_shell` (one live seam call, same lane) retire
when that lane lands, and `set_history_provider`'s
Executor row stays 0 because the install moved to the
§2.3 face (its count there reads 1 since the GAP-10 host
adoption landed). The §2.1 drift the same wave corrects
is recorded on its rows (`CommandNode` 14 → 11,
`pretty_print_command` 1 → 0, the tracked script-driver
arm 1 → 0, `bytes_to_script_text` 1 → 3).

Note what the retained set is made of: the bind registry and the
design-§3.2 AST routes (`execute_ast`, `execute_command`), the
classification plumbing of the routes that stay direct
(`take_parse_error`, `take_exit_jump_pending`,
`take_signal_discard_pending`, `last_command_inverted`), the
full-table snapshots (`env_vars`, `env_vars_snapshot` — the
contract's `environment()` is exported-only), the post-build
history-provider install, the exit-trap funnel the REPL finish
composes by hand, and the read probes in `&self` host methods
(the contract's state views are `&mut`-shaped; the 7 lib-code
`get_env` seam reads were exactly those probes). The read side
still leads what remains — but the bulk of it (102
`resolved_value` calls) now goes through the variables view.
GAP-6 (niubash#863) retired the two read classes this note
named: the full-table snapshots moved to
`Session::environment_full` and the `&self` probes to
`Session::variable_value`; the `env_vars` / `env_vars_snapshot`
rows and the `get_env` row read 0 (see the retirement
paragraph above).
GAP-10 (niubash#863) retired the install this note named:
the post-build history-provider install is the
`Session::set_history_provider` contract face (see the
retirement paragraph above).
GAP-13 (rubash#2833) re-counts the composition this note
names: the live retained set is ten methods —
`execute_ast`, `take_parse_error`, `take_signal_discard_pending`,
`alias_expansion_enabled`, `run_bind_x_command`,
`bind_readline_variable`, `script_fd0_line`,
`last_exit_code`, `builtin_command_names`,
`find_script_on_path` — and every other member this note
lists reads 0 now (`execute_command`,
`take_exit_jump_pending`, `last_command_inverted`,
`env_vars` / `env_vars_snapshot`, `get_env`, the install,
the exit-trap funnel; see the recount paragraph above).
The note's composition list above is kept as migration
history; the recount paragraph is the live record.

### 2.3 `Session` faces niubash calls (Ledger v4 census)

The migrated consumption, same counting rule (occurrences across
`crates/` + `src/`, test modules included). Counts are production
occurrences; a test-module read is noted in the row's prose, not
counted — the caliber the rows already practice (e.g.
`Session::variable_value` reads 7, the production sites, with
its test-module reads named separately in the row):

| Face | Calls | Notes |
| --- | --- | --- |
| `Session::builder()` + `SessionBuilder::{env, login_shell, argv0, disable_builtin, shell_root, winuxcmd_path, compatible_shell_path, build}` | 14 | construction of every `Shell`; the login pair is the builder's `login_shell` (the engine's `mark_login_shell` funnel), replacing the host's hand copy |
| variables view: `resolved_value` / `set` / `unset` | 102 / 39 / 14 | the designed forms of `get_env` / `set_env` / `unset_env` |
| options view: `set` | 26 | both option families by name |
| `Session::last_status` / `set_last_status` | 11 / 14 | |
| `Session::set_positional` | 4 | |
| `Session::inherit_process_stdin` | 5 | the contract method, not the builder stream binding §4 anticipated |
| `Session::expand_prompt` / `run_prompt_command` | 4 / 1 | |
| `Session::run` | 2 | the tracked script arm and, since GAP-2 (rubash#2800 / niubash#898), the unclosed arm — both fold their stop facts from the outcome classification |
| `Session::take_classification` | 2 | GAP-2 drain: the stdin feeder stop rule and the PROMPT_COMMAND tail read; returns the classification values (`exit_jump_raw` / `parse_error` / signal discard) drained from the executor pendings |
| `Session::bind_view` | 7 | GAP-3 bind view: the registry snapshot (`BindView` — generation / editing mode / readline variables / removal tombstones / entries with canonical bytes, kind, resolved keymap and GNU dump line); adopted by the GAP-3 host migration — shell binding load/staleness/edit-mode, bind -x lookup, repl dump handlers and keyseq-timeout read the view, and the host's tier-C registry reads plus the `bind_surface.rs` conversion layer are deleted |
| `Session::completion_view` | 2 | GAP-4 completion view: the completer's live state as one `&self` capture (`CompletionView` — session cwd / the whole variable table in `env_vars_snapshot`'s rendering, attribute pseudo-entries included, sorted by name, with its content fingerprint / function names / open fd numbers); adopted by the GAP-4 host migration — the completer's capture and the highlighter's known-command probe read the view at use time, and the host's `CompletionState` env/function/fd snapshot fields plus their per-line re-synchronization are deleted |
| `Session::has_function` | 1 | GAP-4b: the `&self` function probe (the executor's own lookup; `StateViews::has_function` is the `&mut`-shaped form); adopted by the GAP-4 host migration — the widget-availability probe reads it live, and the tier-C `executor().has_function(name)` read is deleted |
| `Session::resolve_shell_path` | 7 | GAP-5 path resolution: the `&self` instance form of the `executor::path` resolver (the executor's own method over the session's live environment); adopted by the GAP-5 host migration — the host's tier-C `executor().resolve_shell_path(...)` reads (the stdin script path, the login profile paths, env-file resolution, the completer's `/tmp` base in both its capture and its per-line sync, the pwd host/display reads) now go through this face |
| `Session::resolve_shell_path_from_env` | 0 | GAP-5 associated form: resolution against a caller-supplied environment map, delegating to the same `executor::path` function as the executor's associated form; adopted by the GAP-5 host migration as the primitive under the host's `resolve_shell_path_argument` composition family — that family is `#[cfg(test)]`-gated today, so the production count is 0 and the remaining calls (the two family primitives and one test call) are test-module occurrences |
| `Session::resolve_shell_path_process_entries_from_env` | 2 | GAP-5 PATH-entry form: one shell PATH entry resolved to its native directories against a caller-supplied environment map, same source as the executor's associated form; adopted by the GAP-5 host migration — the host's PATH-entry reads (shell.rs's process-path conversion and syntax_highlighting.rs's PATH-dir read) go through this face |
| `Session::shell_pwd_display_path` | 1 | GAP-5 display form: the module function `rubash::executor::shell_pwd_display_path` under a `Session` associated name; adopted by the GAP-5 host migration — the host's direct module-function call (the persona pwd display read) moves to this face |
| `Session::environment_full` | 2 | GAP-6 full-table read: the whole variable table in `env_vars_snapshot`'s rendering (attribute pseudo-entries included), sorted by name, through the one rendering shared with `CompletionView`'s `env` component; adopted by the GAP-6 host migration — the process-PATH synchronization's conversion context and the `NIU_SEGMENT_` prompt mirror read the full table through this face (the mirror's name filter cannot match an attribute pseudo-entry key), and the §2.2 `env_vars` / `env_vars_snapshot` rows retire to 0; the new GAP-6 routing pin reads this face once from the test module |
| `Session::variable_value` | 7 | GAP-6 single-value probe: the stored value of one variable — the `&self` form of the executor's raw table read (`get_env`), deliberately not the `$name` expansion read (`resolved_value` keeps dynamic getters, nameref resolution and the typed-cell layer; the GAP-6 consumer pins record both readings per shape); adopted by the GAP-6 host migration — the 7 lib-code `get_env` seam reads in `&self` host methods (`THIS_SH`, `__RUBASH_POSIX_MODE`, the env-file expansion probe, `PS1`, `NIU_TITLE`, `PWD` twice) move to this face, and the §2.2 `get_env` row retires to 0; the new GAP-6 routing pin reads this face twice from the test module |
| `Session::begin_completion_attempt` | 3 | GAP-7 guard opener: opens one `CompletionAttempt` per completion attempt from the `&mut Session` the host holds; the guard drives the attempt's execution faces (`complete_detailed`, `call_function_with_env` for the widget/compdef callbacks) and lends `&Session` reads (`session()`) live at every nesting depth. Adopted by the GAP-7 host migration as the attempt object replacing the thread-local lend (one install point, seven reads — see the GAP-7 retirement paragraph in §2.2): the two attempt drivers in `Shell` (`drive_completion` / `drive_completion_specific`, serving the probe surface and, through `EditorShellHandle`, the editor flavor) and the highlighter's short view attempt open this face in production; the GAP-7 pins and the reshaped compdef test open it six more times from test modules |
| `Session::run_stdin` / `run_interactive_stdin` | 1 / 1 | |
| `Session::begin_finish` | 3 | GAP-8 finish guard opener: opens one `Finish` per finish from the `&mut Session` the host holds, with the route's pending status as a value; the guard's coarse steps (`run_exit_trap` — pending status in, resolved status out; `final_cwd` — the post-trap directory read; `run_stream_end_duties` — the finish's non-trap duties with the engine history step gated per route through `FinishDuties`; `finish_all` — the one-call pass) decompose the one finish implementation around the host's interleaved duties, and `FinishOutcome` reports the finish boundary as values (status / final cwd / trap ran / history written / duties run). Adopted by the GAP-8 host migration (see the GAP-8 retirement paragraph in §2.2): the count is the three `Shell` funnel sites in shell.rs, by route — the one-shot funnel (`finish_with_engine_finish`, the guard's `finish_all`), the non-consuming trap step (`finish_exit_trap`, driven by the REPL session end, the startup exit, and through the session end the REPL tails), and the duties step (`finish_stream_end_duties`, driven by the REPL session end and the startup exit with the engine history write gated off). `Session::close` / `close_with_report` are already re-based on it engine-side, and the GAP-8 consumer pins open it from test modules |
| `Session::close_with_report` | 0 | GAP-8 (niubash#863): ENTERS the ledger with this record — a pub tier-A-shaped face recorded nowhere before (the GAP-8 scoping finding). The report form of `Session::close` (rubash#2039): the final status plus the post-trap cwd as a `CloseReport` value; re-based on the `Session::begin_finish` guard's one-call pass in the GAP-8 engine PR (adopt-and-extend, captain adjudication D4). No niubash consumer yet; consumed in-repo by `Session::close` and the issue2039 close pins |
| `Session::prepare_line` | 1 | REPL history expansion |
| `Session::complete` / `complete_detailed` / `has_compspec` | 0 / 0 / 0 | GAP-7 (niubash#863): the completion bridge this row used to name is retired — no direct `Session::complete` / `complete_detailed` calls remain; the generation call routes through the completion attempt's `complete_detailed` (counted under `Session::begin_completion_attempt` above) and the compspec gate was rewired by the B5-11 host migration (niu #926, merge `d4879b4b`) onto `Session::has_compspec_at` (counted under that row below) — the name form's production callers are now zero (the two `shell.rs` test-module reads remain, noted in-row, not counted). `CompletionOutcome`/`CompletionPolicy` (rubash#2573) reach the host as the guard's outcome, consumed by niubash#837. GAP-11 (niubash#863): each `Candidate` now carries the engine-side kind its generator knew (`CandidateKind`: `Directory` / `Symlink` / `Executable` / `File`, or `None` when unclassifiable from in-hand evidence), marked at generation under the generator-classification principle; the attempt's filename flavor rides the new `CompletionPolicy.filenames_intent` bit. Recorded staged ahead of the GAP-11 host adoption, so the counts are unchanged — the channel's consumers are already counted here |
| `Session::aliases` / `functions` | 1 / 2 | snapshots for completion state |
| `Session::close` | 0 | GAP-8 (niubash#863): re-based on the `Session::begin_finish` guard's one-call pass engine-side (signature unchanged); the host's one-shot funnel used to be this row's one consumer and now opens the guard directly (see the GAP-8 retirement paragraph in §2.2), so the host count reads 0 — the host no longer consumes a session to finish it |
| `Session::token_view` | 4 | GAP-9 (niubash#863): the lexer projected onto caller-fed text — a flat `TokenView` span stream (absolute byte spans, projected kinds, dequoted values built from the raw slices, command positions and roles folded at projection time from the lexer's stamps and the engine's builtin table, comment spans synthesized from inter-token text), tokenized in the session's posix mode. The contract replacement for the host editor's two private tokenizers and its direct tier-B lexer reads. Adopted by the GAP-9 host migration — the completer's two attempt pipelines (`complete_specific_with_engine`, `complete_input`) capture the attempt session's view into the context, the stateless oracle session reads it for session-less contexts (the tokenize source the highlighter's shell-less instances share), and the highlighter's live arm reads the editor shell's session view; the REPL validator's direct tier-B `tokenize` read is deleted with the H4 retirement, so the §2.1 `lexer` row's `tokenize` count retires 2 → 1. The two extglob pins read the face from test modules (the extglob-on session view and the oracle view) |
| `Session::needs_more_input` | 3 | GAP-9 (niubash#863): ENTERS the ledger with this record — the §6.5 completeness face (`stdin_source_needs_more_posix` as a session query, in the session's posix mode) has had its §4 target home since the RFC but was recorded nowhere (the GAP-9 scoping finding). Adopted by the GAP-9 host migration — the REPL validator reads it through the editor shell handle (a plain `&self` read, never a completion attempt), `PendingReplInput::is_complete` reads it for the validator's pending input, and `is_script_input_complete` reads it for the script-input check; the host's `scan_repl_input` walk plus its heredoc arm, token walk and `__RUBASH_HD1__` marker sniff are deleted. Verdict parity with the host's scanner walk is pinned over the adjudicated corpus in the GAP-9 consumer pins (the Q-B differential stands at 66 of 66 AGREE since the round-2 adjudication classified both divergence classes DIVERGE — engine reader governs — and fixed the face; see the header addendum). The repl completeness pins read the face from the test module |
| `Session::set_history_provider` | 1 | GAP-10 (niubash#863): ENTERS the ledger with this record — the post-build history-provider install as a contract face: a pure lift of the executor setter (replace-always, unguarded), the tier-A home of the install the interactive single-stream discipline used to reach through the tier-C seam (the §2.2 `set_history_provider` row retires 1 → 0 with this record; retained-seam row 13 closes). The GAP-10 host adoption landed with niu #917 (merge `413847b7`): the interactive entry installs through the face (`shell.rs:1125`), so the count is 1 — the staged 0 this row carried until the lane landed is retired by the GAP-13 recount wave (rubash#2833); consumed in-repo by the GAP-10 consumer pins (install reaches the history builtin and history expansion under provider precedence; second install replaces the first; the no-provider default is unchanged). Builder injection (`SessionBuilder::history_provider`) is the build-time spelling of the same install; the face's lifecycle is the setter's — command-substitution executors cloned before a re-install keep the handle they cloned with |
| `Session::history_entries` / `set_history_entries` | 0 / 0 | GAP-10 Q-D (niubash#863): ENTER the ledger with this record — tier-A faces over the engine's `SessionHistory` list (the script plane), recorded nowhere before (the GAP-8 `close_with_report` debt class). No production niubash consumer; the GAP-10 install pin reads `history_entries` once from the test module (`shell.rs:6395`, asserting no second engine list exists while a provider is installed). The faces are the script plane's list spelled as a contract, distinct from the provider plane the install face above serves |
| `Session::apply_invocation_before_startup` / `finish_invocation_startup` | 0 / 0 | GAP-12 (niubash#863): ENTER the ledger with this record — the post-build invocation apply as a contract pair: the apply face (the GAP-7-deferred apply, paired here with its read face) applies a parsed `ShellInvocation` before startup files run and retains it, so the finish face can activate its restrictions after the startup chain without the host holding a clone (the host's `startup_invocation` field deletes). The apply arms the typed interactive flag the apply body alone never writes (the hop-2 asymmetry, absorbed as the apply face's own write). A failed apply retains nothing. Recorded staged ahead of the GAP-12 host adoption, so the counts are 0 (niubash applies through the tier-C seam today); consumed in-repo by the GAP-12 consumer pins (apply→live-read pairing, two-phase restricted split, finish-without-apply no-op, failed-apply no-retention) |
| `Session::applied_invocation_shape` | 0 | GAP-12 (niubash#863): ENTERS the ledger with this record — the live-shape read face: the executor's typed invocation state read back as a value, never the exported carriers (rubash#2129). Declared (`Session::invocation_shape`) vs live is an explicit pinned distinction. Recorded staged ahead of the GAP-12 host adoption, so the count is 0; consumed in-repo by the GAP-12 consumer pins (declared-vs-live divergence, mark corrections, poisoned-env read safety) |
| `Session::remove_env` | 0 | GAP-12 (niubash#863): ENTERS the ledger with this record — the restore-grade removal as a contract face: a pure lift of `Executor::remove_env` (env-table removal + typed-flag clearing for the carriers + journal record + typed cell preserved). NOT the `unset` builtin's leaf — the side-by-side differential (readonly / typed-cell / carrier × both faces) is pinned in the GAP-12 consumer pins. The §2.2 `unset_env` row reads 0 with this record (its recorded count was 1; the true pre-migration count is 3 — see the §2.2 retirement paragraph). The journal is pinned only as this face's implementation semantics (a removal is journaled; Drop restores the birth value); no journal read/drain face exists. Recorded staged ahead of the GAP-12 host adoption, so the count is 0 |
| `Session::mark_login_shell` | 0 | GAP-12 (niubash#863): ENTERS the ledger with this record — the login funnel as a companion of the apply face: a pure lift of `Executor::mark_login_shell` (carrier + `login_shell` shopt together). The route classification that decides to call it stays host knowledge. The §2.2 `mark_login_shell` row retires 1 → 0 with this record. Recorded staged ahead of the GAP-12 host adoption, so the count is 0; consumed in-repo by the GAP-12 consumer pins |
| `Session::glob_filename` | 1 | GAP-13 (rubash#2833): ENTERS the ledger with this record — the filename glob as a contract face: the engine's own glob over the session's live state, consumed by the host completer's glob arm (`completer.rs:540`, through the attempt guard's `session()` lend). Recorded nowhere before (the ten-round re-audit's finding 2): the completeness half of the tripwire (§5.4) scans `rubash::…` path tokens, and a receiver-form method call names no such token |
| `Session::notify_terminal_size` | 1 | GAP-13 (rubash#2833): ENTERS the ledger with this record — the terminal-size notice as a contract face: the resize arm tells the session the new size (`shell.rs:2944`, `session_mut().notify_terminal_size(columns, rows)`), so the engine's `LINES` / `COLUMNS` follow the host's terminal. Recorded nowhere before (the ten-round re-audit's finding 2; same blind spot as the glob face above) |
| `Session::expand_shell_words` | 1 | GAP-13 (rubash#2833): ENTERS the ledger with this record — the shell-words expansion as a contract face: the host's word expansion over live session state (`shell.rs:3360`, `session().expand_shell_words(&text)`). Recorded nowhere before (the ten-round re-audit's finding 2; same blind spot as the glob face above) |
| `Session::environment` | 1 | GAP-13 (rubash#2833): ENTERS the ledger with this record — the exported environment table as a contract face: the session's exported variables as a value (`repl.rs:1131`, the `tilde-expand` readline command collects it to expand the word prefix over the session environment). Exported-only — distinct from `Session::environment_full` (the GAP-6 full table with attribute pseudo-entries). Recorded nowhere before, and caught by the Session-method census (§5.5) itself — the careful human audit of the re-audit missed it, which is the census's founding evidence |
| `Session::call_function_with_env` | 1 | GAP-13 (rubash#2833): ENTERS the ledger as a §2.3 row with this record — the function call with an environment channel as a `Session` face (the §6 entry predates it). The host's widget driver calls it at `shell.rs:3462` (`session_mut().call_function_with_env(...)`); the attempt-guard form (`CompletionAttempt::call_function_with_env`) is the GAP-7 guard's method, counted under `Session::begin_completion_attempt` above (`completer.rs:1093`), and the GAP-7 pins call the guard form twice from test modules. The §2.2 `call_function_with_env` row retires 1 → 0 with this record — the Executor-form call is gone; the remaining leg is this Session face |
| `Session::register_binding` | 1 | GAP-13 (niubash#863): ENTERS the ledger with this record — the bind assembly write as a contract face: the engine converts the `BindKindView` and constructs the `BindEntry` itself, into the same registry pool the `bind` builtin writes (keymap None resolves at registration; a register clears the `bind -r` tombstone for the same keyseq/keymap; same-key replaces in place; generation +1 per call). The GAP-13 host adoption landed with niu #921 (merge `9388c9cf`): the plugin macro path registers through the face (`shell.rs:3197`, `register_macro_binding`), so the count is 1 — the staged 0 this row carried until the lane landed is retired by this record; consumed in-repo by the GAP-13 consumer pins (read-back through `bind_view`, re-register replace + generation, tombstone clear, face writes visible to the `bind` builtin's dumps). The host's one production write site (`bind_surface.rs:46`) retired with the host lane (Q-E); the `BindEntry` / `BindKind` internal demotion in the engine has not landed — the symbols stay `pub`, recorded nowhere after this record. |
| `Session::initialize_readline` | 1 | GAP-13 (niubash#863): ENTERS the ledger with this record — the inputrc read as a contract face: a pure lift of `Executor::initialize_readline` (GNU bashline.c:451); ordering — after rc, before the first snapshot — is the caller's duty and the face does not enforce it; idempotent by the engine flag; missing files silent. The GAP-13 host adoption landed with niu #921 (merge `9388c9cf`): the engine-bindings load reads inputrc through the face (`shell.rs:3164`, `load_engine_bindings`), so the count is 1 — the staged 0 this row carried until the lane landed is retired by this record; consumed in-repo by the GAP-13 consumer pins (inputrc vars + bindings land in the view; second call is a no-op after a file change) and the U-1 probe (first call with HOME empty poisons the flag — GNU parity, bashline.c:451-456/:646). The host's one production call site (`shell.rs:3145`, `load_engine_bindings`) retired with the host lane (line number now 3164). |
| `Session::is_known_keymap` | 1 | GAP-13 (niubash#863): ENTERS the ledger with this record — the keymap membership probe as a contract face: plain `KEYMAP_NAMES` membership, no alias folding (folding lives in the register path inside the engine — the `[keymap]` parser's plain membership, niubash#863 GAP-13 Q-F/U-3). The GAP-13 host adoption landed with niu #921 (merge `9388c9cf`): the `[keymap]` parser probes through the face (`repl.rs:5276`, `parse_keymap_bind_rule`), so the count is 1 — the staged 0 this row carried until the lane landed is retired by this record; consumed in-repo by the GAP-13 consumer pins (all eight names true, fake names false) and by the host pin, which calls the face twice from the test module (production count is 1; the two test-module calls are noted, not counted). |
| `Session::applied_invocation_shopt_flag` | 0 | GAP-12 follow-up (niubash#863, rubash#2848): ENTERS the ledger with this record — the retained invocation's explicit shopt flag as a contract face: the last `-O`/`+O` entry for a name in the invocation applied through `Session::apply_invocation_before_startup`, or `None` when the invocation said nothing about it (and `None` when no invocation was applied). The host needs the explicit-vs-silent distinction the shape read cannot carry: `enter_interactive` arms `expand_aliases` for an interactive session unless the invocation explicitly turned it off (`+O expand_aliases -i` must win over the interactive default; the three GNU oracle arms are pinned in the GAP-13 consumer pins). Recorded staged ahead of the host adoption, so the count is 0; the host follow-up that deletes its `startup_invocation` field rides on this face plus `applied_invocation_shape` |
| `Session::has_compspec_at` | 1 | B5-11 (niubash#863, rubash#2856): ENTERS the ledger with this record — the position-aware compspec gate: whether a compspec governs completion at `(line, cursor)`, answered with the same split the generation path uses (`split_for_completion` + `resolve_compspec`), so the gate and the candidates cannot disagree. The command-name form `has_compspec` cannot answer it: the `-E` empty line has no command and the `-I` initial word IS the command being completed. Adopted by the B5-11 host migration (niu #926, merge `d4879b4b`): the `completer.rs` routing gate reads this face (the `-E`/`-I` gate was unreachable through the name form), so the count is 1 — the gate's single production call |
| `Session::executor` / `executor_mut` | seam door | every §2.2 call goes through these |

Deliberate non-adoptions, adjudicated during the migration:
the `HistoryProvider` is **not** builder-injected by
niubash — builder injection exists
(`SessionBuilder::history_provider`), and niubash's
non-adoption of it stands: a construction-time provider
would exist on script routes too, breaking the host's
single-stream history discipline. The post-build install
home is the Session face, `Session::set_history_provider`
(GAP-10, niubash#863): niubash installs at interactive
entry through the face, preserving the same discipline. `SessionEvents` is
not injected (no host consumer; the host's hooks are host-side
by charter). The builder's process-exit role is left at its
default (library role): niubash's exit semantics are the host
funnels', exactly as under direct `Executor::new()`.
`Session::source_startup_files` / `RcPolicy` are **not
adopted**: the host's startup chain selects files by host
policy (primary-only rc resolution, the login chain,
host-expanded `$ENV`), interleaves the generated plugin
loader at three branch-specific slots under a guard keyed on
the rc text, warns and continues per file, and interleaves
branch tails between files — no class decomposition expresses
that without behavior change, so the per-file funnel stays
direct (through the seam).

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
its §6 symbols stay frozen exactly as recorded. Ledger v4 status:
the migration has landed on niubash's migration branch — §2.3 is
its census, §2.2 the retained seam set, and §2.3's closing notes
record the deliberate non-adoptions (niubash's non-adoption
of the builder's history-provider injection, `SessionEvents`,
`source_startup_files` / `RcPolicy`). The workspace
split (PLAN row 4) remains post-release: it moves `rubash::api`
to the split core crate without changing its shape.

| Consumed family | Tier | Target home |
| --- | --- | --- |
| `Executor` lifecycle + `new` | A | `Session::builder()` — one direct `new` persists outside the builder: the bare executor niubash's invocation-input face borrows for `find_script_on_path` (niubash#694; recorded in the 2026-10-29 backfill) |
| env methods (`get_env`, `set_env`, `unset_env`, `export_env`, `env_vars_snapshot`) | A | `Session::variables()` view; the read side is staged for GAP-6 retirement — the `&self` `get_env` probes move to `Session::variable_value` and the full-table `env_vars_snapshot` reads to `Session::environment_full` (see the header addendum). GAP-12 (niubash#863) corrects this row's "variables view is the target home" claim for the removal half: the removal home is the Session face `Session::remove_env` (a pure lift of `Executor::remove_env`, not the `unset` builtin's leaf on the variables view); the read/write halves stay with the variables view (see the header addendum) |
| option methods (`set_shell_option`, `set_shopt_option`) | A | `Session::options()` view |
| `last_exit_code` / `set_last_exit_code` | A | `Session::last_status` + `RunOutcome` |
| `complete_line` / `complete_line_detailed`, `has_compspec` / `has_compspec_at` | A | `Session::complete` / `complete_detailed` + compspec view (the detailed pair carries the CompletionPolicy of rubash#2573). GAP-11 (niubash#863): the detailed pair's candidates carry `Candidate.kind: Option<CandidateKind>` (filesystem four, marked at generation under the generator-classification principle) and the policy carries the `filenames_intent` bit; `Candidate::new` / `described` default kind to `None` (constructor users source-compatible), struct-literal construction of `Candidate` outside the tree breaks on the new field — a 1.5.1 contract change, with no `#[non_exhaustive]` added. B5-11 (rubash#2856): `has_compspec_at` is the position-aware gate — the host's routing gate for the same generation path, true exactly when a compspec resolves for `(line, cursor)` (`-E`/`-D` empty-line arm, `-I` initial-word arm, own spec else `-D` on argument words); `has_compspec` stays as the command-name form |
| `set_history_provider` + `HistoryProvider` | A | `Session::set_history_provider` — the post-build install face (pure lift of the executor setter, replace-always); builder injection (`SessionBuilder::history_provider`) is the build-time spelling of the same install (trait unchanged) |
| `call_function_with_env`, `has_function`, `functions_snapshot` | A | `Session::call_function` / `Session::call_function_with_env` + functions view (the env channel landed on the contract surface with rubash#2001; until then this row's env half was carried by the tier-C seam below) |
| `inherit_process_stdin` | A | the contract method `Session::inherit_process_stdin` (the builder stream binding §4 anticipated — corrected by the GAP-13 recount wave, rubash#2833; the §2.3 row already read it as the contract method) |
| `execute_ast`, `execute_command`, `execute_prompt_command`, `run_exit_trap_with_status`, `expand_prompt_string_mut`, `set_positional_params`, `take_parse_error`, `take_exit_jump_pending` | A (run path) | `Session::run` + `RunOptions` / `RunOutcome`; the finish-path half of `run_exit_trap_with_status` lands as the `Session::begin_finish` guard's trap step (niubash#863 GAP-8 — see the finish guard row below) |
| `script_driver` run family | A (driver wrappers B) | `Session::run`; thin wrappers stay in the driver crate |
| `pre_process_interactive_line` + `InteractiveExpansion` | A | `Session::prepare_line` |
| completeness (`has_unclosed_input_syntax*`, `script_uses_*`, `check_binary_file`, `heredoc_delimiter_unclosed_quote`, `READ_STDIN_MARKER`) | A / B | `Session::needs_more_input`; helpers in `api::syntax` — the face is recorded with the GAP-9 record (see the header addendum): verdict parity with the host's scanner walk is pinned over the full corpus in the engine's consumer tests — 66 of 66 rows AGREE since the GAP-9 round-2 adjudication classified both divergence classes DIVERGE/engine-governs and fixed the face (the token walk reads the lexer's unterminated-body stamp for heredoc-pending, parse.y:3119 `gather_here_documents`; the composition gained a terminal channel so a token-walk terminal verdict overrides the text half's continuation verdicts, parse.y:6724 `yyerror`), with the parked-feeder verdict fusions making the same cut, so the host lane adopts the face for every corpus shape |
| `usage` printers and option tables | B | stay the shared host/engine bridge |
| `parser` family (`parse*`, `CommandNode`, `ParseLoopOptions`, `WordMetadata`, `ast_print`) | B | `Session::parse` + `api::syntax` view types (no internal `Ast` in Tier A) |
| `lexer` family (`tokenize*`) | B | `api::syntax` token view — the target home is REACHED with the GAP-9 record: the token view exists in `api::syntax` (`Session::token_view`; see the header addendum). The family's remaining host reads retire when the GAP-9 host lane lands |
| `locale` dump face (`collect_locale_strings`, `render_locale_string_dump`) | B | driver-layer text pass over the public lexer (locale.c's home); the rubash binary's main consumes it too |
| `special_paths` registry (`SpecialPathClass`, `classify_special_path`, `special_dev_entries`, `synthetic_proc_dir_entries`) | B | stays in the `special_paths` module in-tree — one registry read by the engine's open points and the host's completion / highlighting faces alike (recorded in the 2026-10-29 backfill) |
| `shell::bind_registry` (`BindEntry`, `BindKind`) | C → **retired (GAP-13)** | interactive layer's own gated surface — **landed as gated in row 11**: niubash consumed it only inside `crates/niubash-runtime/src/bind_surface.rs`, behind niubash's `internals` feature (default on; a `--no-default-features` build has zero Tier C references), converted there to niu-owned mirror types. Retired with the GAP-13 host lane (niu #921, merge `9388c9cf`): `bind_surface.rs` is deleted, the registry's contract home is the Tier A bind view plus the assembly faces (§2.3), and the `BindEntry` / `BindKind` §6 items leave the ledger with this record. |
| `invocation` (`ShellInvocation`, `materialize_command_file`) | B (row-11 ruling) | driver-layer input type: both binaries' `main()` parse through it identically, and the RFC's own §8 puts driver wrappers at B; it stays in the `invocation` module in-tree and moves with the driver layer at the split |
| `builtins::kill` console hooks | C → **replaced (row 11)** | `Executor::inject_signal` / `Executor::interrupt` (Tier A, RFC §6.4) for session-scoped delivery; `script_driver::inject_console_ctrl_c` / `script_driver::consume_background_wrapper_handshake` (Tier B) for the two host-launcher shapes that have no executor yet. The kill items themselves were demoted to `pub(crate)` in the migration's companion change (Ledger v4) |
| session configuration (`set_winuxcmd_path`, `set_shell_root`, `set_compatible_shell_path`, `set_builtin_disabled`, `mark_login_shell`) | A (row-11 ruling) | builder-time session initialization — M-2's own language ("the 55 set_env sites collapse into builder initialization"); these five are the same shape: host facts installed before the first run. GAP-12 (niubash#863): `mark_login_shell` also exists as a post-build Session companion face beside the apply face (the login funnel, carrier + shopt together); the builder-time row above still describes the configuration shape (see the header addendum) |
| invocation shape (`InvocationShape`, `RespawnSource`, `SessionBuilder::invocation_shape`, `Session::invocation_shape`; the Executor accessors `invocation_interactive` / `invocation_login` / `set_invocation_shape`) | A | construction-time session state in `rubash::api`, beside the session-configuration row: the builder applies the declared shape to the executor's typed invocation state at build — the structured declaration is authoritative for the fields it names, over the environment seeds — and `Session::invocation_shape` reads it back; the Executor accessors are the state's engine-side face, behind which the migrated carrier reads of rubash#2130 sit (recorded staged ahead of host adoption; see the header addendum). GAP-12 (niubash#863) adds the post-build pair beside it: `Session::apply_invocation_before_startup` / `finish_invocation_startup` / `applied_invocation_shape`, with `Session::remove_env` / `mark_login_shell` as the removal and login companions (see the header addendum) |
| bind view (`BindView`, `BindEntryView`, `BindKindView`, `BindRemovalView`, `Session::bind_view`) | A | the key-binding registry as one owned snapshot in `rubash::api`, beside the §5.3 state views: the capture reads the registry's own stores (generation, editing mode, the readline variable table, entries, removal tombstones) and renders each entry's canonical bytes with the engine's one `keyseq_bytes` decoder and its dump line with the `bind` builtin's own format helpers, so the contract and the shell cannot disagree; it supersedes the host's tier-C registry reads and handwritten re-decoders when the host lane lands (niubash#863 GAP-3; recorded staged ahead of host adoption; see the header addendum) |
| completion view (`CompletionView`, `Session::completion_view`, `Session::has_function`) | A | the completer's live state as one owned snapshot in `rubash::api`, beside the §5.3 state views: the capture reads the executor's own snapshot functions (session cwd, the full variable table in `env_vars_snapshot`'s rendering with its attribute pseudo-entries, function names, open fd numbers) and derives the env content fingerprint from that rendering, so the contract and the shell cannot disagree; `Session::has_function` is the same executor lookup in `&self` form. It supersedes the host's tier-C completion reads and its per-line re-synchronized snapshot struct when the host lane lands (niubash#863 GAP-4 + GAP-4b; recorded staged ahead of host adoption; see the header addendum). The host's `/tmp` answer (`resolve_shell_path`) is NOT part of this family — the path resolver's face is GAP-5's object |
| path-resolution faces (`Session::resolve_shell_path`, `Session::resolve_shell_path_from_env`, `Session::resolve_shell_path_process_entries_from_env`, `Session::shell_pwd_display_path`) | A | the `executor::path` resolver as `Session` consumption faces in `rubash::api`, beside the §5.3 state views: each face delegates to the one resolver through the `Executor` method (or, for the display form, the module function) that already wraps it, so the contract and the shell cannot disagree about a spelling — the ruled path contract (bare `/` is the payload root, Option B, the drive spellings) does not move. They supersede the host's tier-C resolution reads through the §2.2 seam and its direct module-function call when the host lane lands (niubash#863 GAP-5; recorded staged ahead of host adoption; see the header addendum) |
| variable read faces (`Session::environment_full`, `Session::variable_value`) | A | the `&self` variable reads in `rubash::api`, beside the §5.3 state views: the full table through the one sorted rendering the completion view's `env` component shares (`completion_view::full_env_sorted` over the executor's own snapshot), and the single-value stored read as the `&self` form of the executor's `get_env` — the stored string, not the `$name` expansion read. They supersede the host's remaining tier-C variable reads (the §2.2 full-table snapshots and the `&self` `get_env` probes) when the host lane lands (niubash#863 GAP-6; recorded staged ahead of host adoption; see the header addendum) |
| completion-attempt guard (`CompletionAttempt`, `Session::begin_completion_attempt`) | A | one completion attempt as an explicit, reentrancy-aware object in `rubash::api`, beside the §5.3 state views: the guard owns the `&mut Session` borrow for the attempt, so the host's completer drives the attempt's execution faces through it (`complete_detailed`, `call_function_with_env` for the widget/compdef callbacks, with the temporary bindings the callback contract already carries) and reads live state through its `&Session` lend (`session()` — the `&self` read faces, never dark under nesting). Nested attempts are repeated calls on the same guard; a second guard cannot exist while one lives, so the aliasing hazard the thread-local's reentrancy flag papered over is a compile error. It supersedes the host's thread-local shell lend (`COMPLETION_BRIDGE`) when the host lane lands (niubash#863 GAP-7; recorded staged ahead of host adoption; see the header addendum). The completion window itself is a depth counter, not a bit, so an inner attempt returning never closes the outer attempt's `compopt` window |
| finish guard (`Finish`, `Session::begin_finish`) | A | one session finish as an explicit, step-decomposed object in `rubash::api`, beside the §5.3 state views: the guard owns the `&mut Session` borrow for the finish and takes the route's pending status as a value, so the host drives the finish's coarse steps through it (`run_exit_trap` — the pending status in, the resolved status out; `final_cwd` — the post-trap directory read, the value that exists only between the trap and consumption, rubash#668; `run_stream_end_duties` — the history write gated per route, the stream-end job flush, the debugger note, the procsub close, the hangups; `finish_all` — the one-call pass) and interleaves its own duties (the post-trap cwd record, the host-provider history flush, the process exit) between them, reading the finish boundary back as values (`FinishOutcome`: status / final cwd / trap ran / history written / duties run). The session is not consumed, so the interactive route — session in a shared cell, history in the host provider's store — finishes through the same guard the consuming `Session::close` / `close_with_report` are re-based on. Order: the plain path follows GNU `exit_shell` (the EXIT trap before the history save, shell.c:1000-1009; captain adjudication D1); the termsig sub-order (sig.c:590-667) is preserved exactly. It supersedes the host's five hand-composed finish funnels and its tier-C trap-step seam reads when the host lane lands (niubash#863 GAP-8; recorded staged ahead of host adoption; see the header addendum) |
| history provider install face (`Session::set_history_provider`) | A | the post-build history-provider install as a `Session` setter in `rubash::api`, beside the §5.3 state views: a pure lift of the executor setter (replace-always, unguarded — install-once remains the host's guard), so the interactive single-stream discipline installs through the contract instead of the tier-C seam; builder injection is the same install at build time. It supersedes the host's seam install when the host lane lands (niubash#863 GAP-10; recorded staged ahead of host adoption; see the header addendum) |
| driver path resolution (`resolve_shell_path`, `resolve_shell_path_from_env`, `resolve_shell_path_process_entries_from_env`, `find_script_on_path`, `shell_pwd_display_path`) | B (row-11 ruling) | driver helpers beside `script_driver`: the host/driver layer's path machinery, consumed identically by both binaries' startup and script lookup; the resolver faces' `Session` consumption forms are tier A — see the path-resolution faces row above — and `find_script_on_path` has no Session form |
| signal/run state accessors (`take_signal_discard_pending`, `last_command_inverted`, `ExecuteError::SigintUnwind`) | A (row-11 ruling) | part of the run path's outcome surface (`Session::run` + `RunOutcome`): the host must be able to read how a run ended and service the engine's signal bookkeeping |
| snapshots & misc accessors (`aliases_snapshot`, `alias_expansion_enabled`) | C (case-by-case) | promote only if a later census shows a second consumer; otherwise internals. `bind_registry_generation` rides the gated bind surface (§4's bind row), not this one; `bind_registry_snapshot` was demoted to crate-internal with #2853 and rides no surface (see header addendum) |
| tier-C executor seam (`Session::executor` / `executor_mut`) | C (Ledger v4) | the recorded door for the retained direct faces (§2.2's set): the bind registry, the design-§3.2 AST routes, the run-classification plumbing of the retained direct routes, the full-table snapshots, and the `&self` probe reads the `&mut`-shaped views cannot serve. (The post-build provider install left this set with GAP-10: it is the `Session::set_history_provider` face.) (The post-build invocation faces left this set with GAP-12: the apply/read pair plus the removal and login companions are `Session` faces now.) Added with the migration's companion change; its consumer set is expected to shrink, never grow |
| debugger front doors (`run_dap_stdio`, `run_debug_cli`, `DapLaunchPreset`, `DebugCliPreset`) | B | driver-layer front doors at the crate root — the design's §10 forms of the one debug session; they stay driver surface, not `Session` methods, and move with the driver layer at the split |
| `builtins::bind` spec helpers (`BindingSpec`, `BindSpecError`, the `parse_*` / `format_*` / `strip_*` helpers; plus the `InputrcEnv` / `parse_inputrc_with_env` inputrc reader pair; plus `READLINE_FUNCTIONS` and the `format_macro_plain` / `format_variable_plain` plain-form formatters, recorded in the 2026-11-01 backfill) | C | ride the gated bind surface with the `shell::bind_registry` row above: consumed only inside niubash's `bind_surface.rs` behind its `internals` feature — except the inputrc reader pair, which niubash-runtime's config module consumes directly at startup, and the 2026-11-01 trio, which niubash-runtime's readline_commands / repl modules consume directly for the host funmap and the dump-macros / dump-variables faces (see the §2 row) |
| `history_expand` engine (`HistChars`, `HistCtx`, `HistEngineState`, `HistLookup`, `history_expand`) | B | stays in the `history_expand` module in-tree — the engine's own history faces and the host's readline `history-expand-line` face drive the same engine (recorded in the 2026-11-01 backfill) |
| `expand::tilde` (`tilde::expand_word_prefix`) | B | stays in the `expand::tilde` module in-tree — the engine's expansion faces and the host's readline `tilde-expand` command share the one tilde expander (recorded in the 2026-11-01 backfill) |

History division (R-A5, niubash#863 GAP-10 annotation on the
history row above): GNU has one history list; this contract
has two planes, switched by provider presence, and the
semantics divide per row — numbering base: the engine list
advances its `base` under stifle (history.c:389-404,
675-698), a provider that stifles its store must advance
its `history_base` face the same way (the trait default 1
is the unstifled value, rubash#2007); stifle: capacity on
the engine list, the host's capacity/view discipline on
its store; timestamps: the list's parallel stamps vs the
provider's `entry_timestamps` channel (the engine's
dispatch-time stamp ledger stands in for an empty
channel); session rules: the engine's append-vs-write
accounting (bashhist.c:488, 440-441) on its plane, the
ruled GNU session model (read once, isolation, `-n`/`-r`)
as the host store's discipline on its plane, with the
session-end flush a host duty whose order the GAP-8
finish guard pins; record filters: the bashhist.c gates
on the engine recording path, HISTCONTROL/HISTIGNORE
under the host's settings snapshot before entries reach
its store; file format: `#<epoch>` stamp lines on
timestamped writes, with plain faces kept for stores
that cannot carry them (niubash#646); the
`-a`/`-w`/`-r`/`-n` faces: one builtin over the plane's
list; `last_line_added`: the list's flag vs the
provider's save-at-accept report (bashhist.c:174, :764,
:961; fc.def:328-335); HISTFILE retarget: a host
file-placement duty (niubash#661), no contract face. The
table is written in full in the `src/history.rs` module
doc beside the trait; its `history_base` /
`last_line_added` rows rest on the GAP-10 probes U-2/U-3
against GNU 5.3.20 (byte-identical).

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

5. **Session-method census** (same script, `--niu` mode;
   GAP-13, rubash#2833): the completeness half above scans
   `rubash::…` path tokens, which cannot see receiver-form
   method calls (`session().glob_filename(...)` names no
   `rubash::` path) — the blind spot the ten-round re-audit
   documented, and the reason three consumed Session faces
   went unrecorded for a full release cycle. The tripwire's
   `--niu` mode therefore also enumerates every `pub fn` in
   the engine's `impl Session` block and scans the paired
   niubash tree for receiver-form calls of each name
   (`.<name>(` after a session / guard lend, plus the
   `Session::<name>(` associated spelling), failing on any
   called-but-unrecorded name except the curated
   `AMBIGUOUS_SESSION_METHODS` list — names that collide
   with unrelated calls in the host (founding entry:
   `parse`, which the host calls fifteen times as
   `FromStr::parse` on strings and numbers, never as
   `Session::parse`). A name on that list is verified by
   hand on each census run, not by the scan; the list grows
   only by a recorded hand verification. The census is what
   caught `Session::environment` — a fourth unrecorded face
   the careful human audit missed. Guard methods
   (`CompletionAttempt`, `Finish`) are not enumerated here:
   their calls ride under their opener rows in §2.3.

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
item rubash::api::BindEntryView
item rubash::api::BindKindView
item rubash::api::BindRemovalView
item rubash::api::BindView
item rubash::api::CandidateKind
item rubash::api::CompletionAttempt
item rubash::api::CompletionOutcome
item rubash::api::CompletionPolicy
item rubash::api::CompletionView
item rubash::api::Finish
item rubash::api::FinishDuties
item rubash::api::FinishOutcome
item rubash::api::Flow
item rubash::api::InvocationShape
item rubash::api::PreparedLine
item rubash::api::RespawnSource
item rubash::api::RunOptions
item rubash::api::RunOutcome
item rubash::api::Session
item rubash::api::SessionBuilder
item rubash::api::SessionHistoryWrite
item rubash::api::TokenKindView
item rubash::api::TokenRole
item rubash::api::TokenSpan
item rubash::api::TokenView
item rubash::builtins::bind::BindSpecError
item rubash::builtins::bind::BindingSpec
item rubash::builtins::bind::DEFAULT_FUNCTION_BINDINGS
item rubash::builtins::bind::InputrcEnv
item rubash::builtins::bind::READLINE_FUNCTIONS
item rubash::builtins::bind::format_invalid_keymap
item rubash::builtins::bind::format_macro_plain
item rubash::builtins::bind::format_variable_plain
item rubash::builtins::bind::parse_inputrc_with_env
item rubash::builtins::bind::parse_positional_binding
item rubash::builtins::bind::parse_unix_command_spec
item rubash::builtins::bind::strip_set_keyword
item rubash::builtins::complete::JobCandidate
item rubash::decode_to_visible_text
item rubash::executor::ExecuteError
item rubash::executor::bytes_to_shell_text
item rubash::executor::read_process_stdin_bytes
item rubash::executor::Executor
item rubash::executor::shell_pwd_display_path
item rubash::expand::tilde::tilde::expand_word_prefix
item rubash::history::HistoryProvider
item rubash::history_expand::HistChars
item rubash::history_expand::HistCtx
item rubash::history_expand::HistEngineState
item rubash::history_expand::HistLookup
item rubash::history_expand::history_expand
item rubash::invocation::ShellInvocation
item rubash::invocation::materialize_command_file
item rubash::lexer::has_unclosed_input_syntax
item rubash::lexer::has_unclosed_input_syntax_posix
item rubash::lexer::tokenize
item rubash::lexer::tokenize_with_initial_posix
item rubash::locale::collect_locale_strings
item rubash::locale::render_locale_string_dump
item rubash::parser::Ast
item rubash::parser::CommandNode
item rubash::parser::ParseLoopOptions
item rubash::parser::WordMetadata
item rubash::parser::ast_print::pretty_print_command
item rubash::parser::ast_print::pretty_print_input
item rubash::parser::parse
item rubash::parser::parse_with_options
item rubash::run_dap_stdio
item rubash::run_debug_cli
item rubash::script_driver::consume_background_wrapper_handshake
item rubash::script_driver::inject_console_ctrl_c
item rubash::script_driver::stdin_script_errexit_enabled
item rubash::script_driver::READ_STDIN_MARKER
item rubash::script_driver::bytes_to_script_text
item rubash::script_driver::check_binary_file
item rubash::script_driver::heredoc_delimiter_unclosed_quote
item rubash::script_driver::run_script_with_history_at_line_offset_classified
item rubash::script_driver::run_script_with_history_at_line_offset_tracked
item rubash::script_driver::run_source_with_line_offset
item rubash::script_driver::script_uses_aliases
item rubash::script_driver::script_uses_history
item rubash::special_paths::SpecialPathClass
item rubash::special_paths::classify_special_path
item rubash::special_paths::special_dev_entries
item rubash::special_paths::synthetic_proc_dir_entries
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
variant rubash::api::BindKindView::Execute
variant rubash::api::BindKindView::Function
variant rubash::api::BindKindView::Macro
variant rubash::api::CandidateKind::Directory
variant rubash::api::CandidateKind::Executable
variant rubash::api::CandidateKind::File
variant rubash::api::CandidateKind::Symlink
variant rubash::api::Flow::ExitRequested
variant rubash::api::PreparedLine::Discard
variant rubash::api::PreparedLine::Execute
variant rubash::api::RespawnSource::EngineChild
variant rubash::api::RespawnSource::Fresh
variant rubash::api::SessionHistoryWrite::Run
variant rubash::api::SessionHistoryWrite::Skip
variant rubash::api::TokenKindView::And
variant rubash::api::TokenKindView::Append
variant rubash::api::TokenKindView::Assignment
variant rubash::api::TokenKindView::Background
variant rubash::api::TokenKindView::BraceExpand
variant rubash::api::TokenKindView::CommandSubst
variant rubash::api::TokenKindView::Comment
variant rubash::api::TokenKindView::HereDoc
variant rubash::api::TokenKindView::HereDocBody
variant rubash::api::TokenKindView::HereString
variant rubash::api::TokenKindView::Keyword
variant rubash::api::TokenKindView::Or
variant rubash::api::TokenKindView::Pipe
variant rubash::api::TokenKindView::PipeErr
variant rubash::api::TokenKindView::RedirectErr
variant rubash::api::TokenKindView::RedirectErrAppend
variant rubash::api::TokenKindView::RedirectIn
variant rubash::api::TokenKindView::RedirectOut
variant rubash::api::TokenKindView::Semicolon
variant rubash::api::TokenKindView::Variable
variant rubash::api::TokenKindView::Word
variant rubash::api::TokenRole::Assignment
variant rubash::api::TokenRole::Builtin
variant rubash::api::TokenRole::Plain
variant rubash::api::TokenRole::RedirectTarget
variant rubash::api::TokenRole::Reserved
variant rubash::builtins::bind::BindingSpec::Function
variant rubash::builtins::bind::BindingSpec::Macro
variant rubash::executor::ExecuteError::CommandNotFound
variant rubash::executor::ExecuteError::ExitCode
variant rubash::executor::ExecuteError::ExpansionFailure
variant rubash::executor::ExecuteError::FatalFunctionError
variant rubash::executor::ExecuteError::IoError
variant rubash::executor::ExecuteError::Return
variant rubash::executor::ExecuteError::SigintUnwind
variant rubash::special_paths::SpecialPathClass::Filesystem
method rubash::Executor::new
method rubash::api::CompletionAttempt::call_function_with_env
method rubash::api::CompletionAttempt::complete
method rubash::api::CompletionAttempt::complete_detailed
method rubash::api::CompletionAttempt::session
method rubash::api::Finish::final_cwd
method rubash::api::Finish::finish_all
method rubash::api::Finish::run_exit_trap
method rubash::api::Finish::run_stream_end_duties
method rubash::api::InvocationShape::interactive
method rubash::api::InvocationShape::login_interactive
method rubash::api::InvocationShape::new
method rubash::api::OptionsView::set
method rubash::api::Session::aliases
method rubash::api::Session::applied_invocation_shape
method rubash::api::Session::applied_invocation_shopt_flag
method rubash::api::Session::apply_invocation_before_startup
method rubash::api::Session::begin_completion_attempt
method rubash::api::Session::begin_finish
method rubash::api::Session::bind_view
method rubash::api::Session::builder
method rubash::api::Session::call_function_with_env
method rubash::api::Session::close
method rubash::api::Session::close_with_report
method rubash::api::Session::complete
method rubash::api::Session::complete_detailed
method rubash::api::Session::completion_view
method rubash::api::Session::executor
method rubash::api::Session::executor_mut
method rubash::api::Session::environment
method rubash::api::Session::environment_full
method rubash::api::Session::expand_prompt
method rubash::api::Session::expand_shell_words
method rubash::api::Session::finish_invocation_startup
method rubash::api::Session::functions
method rubash::api::Session::glob_filename
method rubash::api::Session::has_compspec
method rubash::api::Session::has_compspec_at
method rubash::api::Session::has_function
method rubash::api::Session::history_entries
method rubash::api::Session::inherit_process_stdin
method rubash::api::Session::initialize_readline
method rubash::api::Session::invocation_shape
method rubash::api::Session::is_known_keymap
method rubash::api::Session::last_status
method rubash::api::Session::mark_login_shell
method rubash::api::Session::needs_more_input
method rubash::api::Session::notify_terminal_size
method rubash::api::Session::options
method rubash::api::Session::parse
method rubash::api::Session::prepare_line
method rubash::api::Session::register_binding
method rubash::api::Session::remove_env
method rubash::api::Session::resolve_shell_path
method rubash::api::Session::resolve_shell_path_from_env
method rubash::api::Session::resolve_shell_path_process_entries_from_env
method rubash::api::Session::run
method rubash::api::Session::run_interactive_stdin
method rubash::api::Session::run_prompt_command
method rubash::api::Session::run_stdin
method rubash::api::Session::set_history_entries
method rubash::api::Session::set_history_provider
method rubash::api::Session::set_last_status
method rubash::api::Session::set_positional
method rubash::api::Session::shell_pwd_display_path
method rubash::api::Session::take_classification
method rubash::api::Session::token_view
method rubash::api::Session::variables
method rubash::api::Session::variable_value
method rubash::api::SessionBuilder::argv0
method rubash::api::SessionBuilder::build
method rubash::api::SessionBuilder::compatible_shell_path
method rubash::api::SessionBuilder::disable_builtin
method rubash::api::SessionBuilder::env
method rubash::api::SessionBuilder::interactive
method rubash::api::SessionBuilder::invocation_shape
method rubash::api::SessionBuilder::login_shell
method rubash::api::SessionBuilder::shell_root
method rubash::api::SessionBuilder::winuxcmd_path
method rubash::api::TokenKindView::is_separator
method rubash::api::TokenKindView::is_word_shaped
method rubash::api::TokenView::last_separator_before
method rubash::api::TokenView::segment_words_before
method rubash::api::TokenView::word_at
method rubash::api::VariablesView::resolved_value
method rubash::api::VariablesView::set
method rubash::api::VariablesView::unset
method rubash::executor::Executor::alias_expansion_enabled
method rubash::executor::Executor::bind_readline_variable
method rubash::executor::Executor::bind_registry_generation
method rubash::executor::Executor::builtin_command_names
method rubash::executor::Executor::call_function_with_env
method rubash::executor::Executor::close_all_persistent_output_process_substitutions
method rubash::executor::Executor::env_vars
method rubash::executor::Executor::env_vars_snapshot
method rubash::executor::Executor::execute_ast
method rubash::executor::Executor::execute_command
method rubash::executor::Executor::export_env
method rubash::executor::Executor::find_script_on_path
method rubash::executor::Executor::get_env
method rubash::executor::Executor::has_compspec
method rubash::executor::Executor::has_compspec_at
method rubash::executor::Executor::has_function
method rubash::executor::Executor::last_exit_code
method rubash::executor::Executor::inject_signal
method rubash::executor::Executor::interrupt
method rubash::executor::Executor::invocation_interactive
method rubash::executor::Executor::invocation_login
method rubash::executor::Executor::last_command_inverted
method rubash::executor::Executor::mark_login_shell
method rubash::executor::Executor::resolve_shell_path
method rubash::executor::Executor::resolve_shell_path_from_env
method rubash::executor::Executor::run_bind_x_command
method rubash::executor::Executor::resolve_shell_path_process_entries_from_env
method rubash::executor::Executor::run_exit_trap_with_status
method rubash::executor::Executor::script_fd0_line
method rubash::executor::Executor::set_env
method rubash::executor::Executor::set_history_provider
method rubash::executor::Executor::set_invocation_shape
method rubash::executor::Executor::set_shopt_option
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
side. Ledger v4, 2026-10-29 (1.5.0 Session migration): niubash's
host consumption migrated onto `rubash::api` (§2.3 is the
Session-side census; §2.2 re-censused to the retained tier-C
seam set, §4 gains the seam row); the `script_driver`
run/stdin/interactive-prep faces and `finish_shell` leave the
consumed surface (the Session routes carry them);
`source_startup_files` / `RcPolicy` adjudicated not adopted
(the host startup chain, §2.3 note); the `builtins::kill`
demotion follow-up landed in the companion change;
`WordMetadata::word_quotes`' last reader dropped its read
(the field's retirement follows engine-side).
Backfill, 2026-10-29: the `special_paths` family is
recorded (§0/§2.1/§4/§6) — public since rubash#2011 and
consumed by niubash#677's completion / highlighting faces,
missed by the v4 census, and flagged by §5's completeness
check once the same-source step went green behind the
migration.
Backfill, 2026-10-29: the constructor path
`rubash::Executor::new` is recorded (§0/§2.1/§4/§6) —
consumed by niubash#694's invocation-input operand reader
(landed by niubash#695) to borrow `find_script_on_path`,
missed by the v4 census (which counted the builder
construction and the receiver-form seam calls), and
flagged by §5's completeness check.
Backfill, 2026-11-01: the bind family's
`READLINE_FUNCTIONS` / `format_macro_plain` /
`format_variable_plain`, the `history_expand` engine
family, and `expand::tilde::tilde::expand_word_prefix`
are recorded (§0/§2.1/§4/§6) — consumed by the
niubash#765-#767 host readline batch through
niubash-runtime's readline_commands and repl modules,
missed by the v4 census, and flagged by §5's
completeness check.
Staged record, 2026-10-31: the invocation-shape family
(rubash#2084 / rubash#2130) is recorded (§2.1/§4/§6)
ahead of host adoption — consumed today by the engine's
own build path, its migrated carrier reads, and the
in-tree consumer tests; niubash names none of it yet.
The same change's per-flag setter
`SessionBuilder::interactive` is recorded in §6 with
the family, pairing `SessionBuilder::login_shell` (v4).
Ledger v1, 2026-10-25. Update rule: any change to the consumed
surface (a symbol appears, vanishes, or moves tiers) updates this
file — both copies — in the same change that causes it, with the
census basis in §0 re-stated.*
