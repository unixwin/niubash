# Changelog

All notable changes to Niubash are documented in this file.

## [Unreleased]

### Fixed

- Upgrades now reconcile the bundled package shims (`winuxcmd\usr\bin\gawk.exe`, `awk.exe`): `wpm links rebuild` cannot reach them on a bundled install because the product ships no `.wpm` state, so replacing `winuxcmd.exe` left those names on the previous image — the 1.4.1 stale `awk.exe`/`gawk.exe` residue of #468. The installer runs `niu --relink-bundled-shims --force` after the wpm rebuild, and shell-startup activation runs the same reconcile: missing or stale declared names are parked and relinked to the current `winuxcmd.exe`, extracted copies are upgraded to real hardlinks, and names owned by the machine's own `.wpm` state (e.g. a user `wpm install awk`) are never touched.

## [1.4.2] - 2026-10-20

Release addressing P0 transient console window pop-ups across WinuxCmd shims, rubash child spawns, and niubash runtime helpers, plus the v1.4.1 golden-journey release audit fixes.

### Fixed

- No more transient console windows when spawning external commands from a windowless or ConPTY host: the bundled WinuxCmd v1.1.18 `winuxcmd-shim` launcher suppresses orphan console creation when the parent has no visible console (#466, WinuxCmd#1282), rubash suppresses console windows on child process spawns from windowless parents (rubash#909), and niubash allocates at most one hidden console per session while adopting/suppressing orphan consoles across `fd::spawn_whitelisted`, `git_prompt`, completion helpers, plugin source git clones, and setup wizard probes (#462, #455).
- `argv[0]`-marked login shells (`-bash`, `-niu`) preserve `shopt login_shell` as `on` across `reset_shell_options` (#465, rubash#911).
- Unix platform shell discovery resolves the installed shell binary by platform binary name (`niu` on Unix vs `niu.exe` on Windows) (#463).
- Golden-journey release audit fixes: ConPTY gallery pseudo-console window preservation (#455), sandbox hygiene and deterministic test gates (#442, #447, #449).
- Release packaging: the product zip now carries `winuxcmd-shim.exe` beside `winuxcmd.exe` so upgrades replace a stale package-shim forwarder (the actual #466 fix payload) instead of leaving the 1.1.x buggy binary in place, and the x64 preinstall leg is fail-closed — a failed pin check, package install, or shim materialization stops the release instead of silently shipping a reduced bundle (#468). The gawk `unixwin2` artifact 404 that silently skipped the 1.4.1 gawk preinstall is fixed at the source (wpm-artifacts asset name).

### Changed

- Release chain: the bundled rubash engine pin moves to `a83a09d32b4a4f953d29af0acc0e781b542206b8` (`1.4.1`) and the bundled WinuxCmd pin moves to `v1.1.18` (#466).

## [1.4.1] - 2026-10-19

Healthy re-release on the pinned release chain (the bundled engine is now locked to an exact verified commit). Details in the linked issues/PRs — entries here are deliberately short.

### Fixed

- Pasted multi-line scripts no longer swallow the typeahead that follows them: the Enter-signature probe re-injects parked keys to the console queue head, so an engine `read` after a paste-submit sees the remaining input (#417, #420, #426, #433).
- `niu -c` and stdin scripts reject a stray `)` exactly like GNU bash (rc 2, same diagnostics/line numbers) instead of silently running the subshell body (#431).
- `rbash` startup semantics match GNU: the restricted face activates after startup files in every invocation form (#413); POSIX `$ENV` `exit` now exits silently without the greeting tail (#422).
- A DSR-silent terminal keeps its shell; TAB completion commits (#418).

### Changed

- Release chain: the bundled rubash engine is pinned to an exact commit (fail-hard on mismatch) and dirty builds self-identify in `niu --version` (#424); the WinuxCmd bundle moves to v1.1.16 (POSIX drive-dialect fixes for mkdir/ln/stat/realpath/readlink) (#427).
- CI: the VCS prompt timing budget is relative to an interleaved spawn baseline — immune to shared-runner noise, still catches real render regressions (#423/#436).
- Engine: rubash 1.4.1 — ERR-trap nested-context semantics, patsub replacement-text escaping, subscript ANSI-C double-state law, pushd/popd readonly + message forms, `${var@P}` status-leak fix, and a package guard against build-variant directories.

## [Unreleased]

### Changed

- **The user rc is read-only to plugin management — the generated loader replaces the managed block** (niubash#359 case A, 1.4.2). The control plane (`~/.niubash/plugins.toml` + `~/.niubash/sources/registry.toml`) now drives a derived activation plane: `niu plugin` verbs render and publish `~/.niubash/generated/plugins.niu` (deterministic, byte-stable when the manifest is unchanged, validate-then-atomic-publish so a failed generation never replaces the last good one and a concurrent reader never sees a missing file — the #304 invariant moves to the loader). The host sources that loader BEFORE the user rc, so user PS1/alias/function overrides win. No verb writes `~/.niubashrc` anymore: `plugin add/enable/disable/sync/update/restore/trust` and theme picks keep their write rights over the sidecar manifest only, `niu setup` stops embedding managed blocks (its whole-file write converges to create-only in 1.4.3), and the rc write-era guards (user-line migration, duplicate collapse, near-marker repair, verb-time backups) retire with the write path — a #324-style corrupt rc is now preserved byte-identically through every verb, with the doctor naming the debris instead of repairing it.
- **Double-execution guard for the migration window**: the loader generation records the managed-block marker pairs it supersedes; at startup the host scans the rc read-only and keeps the loader OFF while any superseded block still executes (rather than run the plugin set twice), with a one-time hint naming `niu plugin migrate-rc`. Removing the old block by hand — the only removal there has ever been — turns the loader on. `niu doctor` reports the transition state read-only.
- **New verb `niu plugin migrate-rc [--diff]`**: the read-only migration diagnostic. It scans the rc for legacy managed blocks (exact and hand-edited marker shapes, with line ranges and a begin-without-end malformed state reported, never fatal), publishes the candidate loader generation from the current manifest, records the superseded markers, and with `--diff` writes the suggested post-migration rc under `~/.niubash/candidates/`. It never modifies the rc — the removal is always the user's own edit.
- **Rollback switch**: `NIU_PLUGIN_LOADER=off` (`off`/`false`/`0`/`no`) stops the host from loading the generated loader — plugins return to manual-source mode per the documented guidance. The switch only disables loading; it never writes anything, and unsetting it rolls forward.
- **Control-plane writers are atomic** (design §4.4): `save_spec` (`plugins.toml`) and `write_source_registry` (`sources/registry.toml`) swap whole files through a same-dir temp + replace rename, so a concurrent reader never observes a truncate window.
- **clippy debt: `needless_borrow` cleared in `niubash-runtime` — first batch of niubash#450**. `history.rs` passed `&mut state` — a `&mut HistoryState` already returned by `lock_mut()` — to `refresh_if_stale`/`sync_state`/`file_signature_lossy`, so each call built a `&mut &mut HistoryState` the compiler immediately dereferenced (11 sites across `reload_from_file`/`clear_memory`/`save`/`update`/`clear`/`delete`); `plugins/assets.rs` passed `&OMB_BLOCK` — a `&str` const — to `seed_legacy_block` in three rc fixtures. With the redundant borrows gone, the six `let mut state` bindings no longer need `mut` either, so it goes too. `cargo clippy -p niubash-runtime --all-targets` reports 14 fewer unique warning sites (119 → 105) with no new lint introduced; the remaining categories (`question_mark`, `chunks_exact_to_as_chunks`, `redundant_closure`, `cloned_ref_to_slice_refs`, …) stay for the follow-up batches the issue tracks.

### Added

- `plugins::loader`: the generated-loader module — document format (banner, superseded-marker record, per-source sections byte-identical to the old block bodies), structural validation, atomic publish, the startup decision (opt-out switch → double-execution scan → load), the legacy-block scanner behind `migrate-rc`/doctor, and the one-time hint marker. Gate cases 359.4–359.9 and the 359.12 structure ratchet are pinned in `tests/rc_readonly_loader.rs`; the retired writers stay in `assets.rs` as an unwired rollback zone (design §4.3) with the ratchet asserting they are reachable from no verb.

- **Shell-face startup is a measured contract** (`scripts/perf/shell-face-startup.py`):
  execution-level `-c exit` timing for every installed face — the `niu.exe`
  payload, the `winuxcmd\bin` / `winuxcmd\usr\bin` `bash.exe`/`sh.exe` links,
  and the `C:\Program Files\Git\bin\bash.exe` convention slot — with a
  verify-before-record rule (a face that cannot execute prints the failure
  note, never a fast number), structural annotations (size, hardlink count,
  same-file-as-payload), a `Measure-Command` cross-check mode, and a
  deterministic `--check-links` gate: every declared face must be the same
  file as the payload `niu.exe`. A measured-time-ratio gate was tried first
  and rejected with data — shimmed faces measured 1.04–1.46x the payload
  while healthy relinked faces measured 0.91–1.26x, overlapping intervals,
  so no ratio separates the states without false kills (niubash#377 CR).
  Measured on the 1.4.0
  install (faces still the 243,200 B rubash forwarders): payload
  `niu -c exit` 27.2 ms avg, faces 42.1–47.3 ms (+9.8 to +15.0 ms, one extra
  process), the .NET slot 80.6 ms (+48 ms, two extra processes); after
  `--relink-shells --force` on the same engine the faces measure equal to the
  payload within noise (23.8–29.7 ms, same inode) with `$0`, `BASH_VERSION`,
  POSIX mode and the `NIU_SHELL` contract verified through the links.
- **The installer sweeps retired shim shapes out of the PATH entry directory**
  (niubash#376 hygiene): `{app}\winuxcmd\bin` and `{app}\winuxcmd\usr\bin` are
  cleared of `*.bak-*` / `*.old-*` / `*.niu-old-*` on install and uninstall —
  the same dead-weight class as the existing `niu.exe.old-*` sweep, observed
  live as `bash.exe.bak-1.4.0` / `sh.exe.old-shposix` (4 × 226,816 B on real
  machines; `*.niu-old-*` covers a relinker crash between park and drop).
  Live entry names are never matched. The PATH entry directory contract
  (hardlinks only, `usr\bin` off `PATH`, declared entry set) is now documented
  in `docs/src/installer.md`.

- **The installer's slot contract is pinned by tests, not by a comment**
  (niubash#398): the ruling that the install tree never claims the
  `C:\Program Files\Git\bin\bash.exe` convention slot — the installer runs
  `PrivilegesRequired=lowest`, cannot elevate, and the slot directory is
  admin-walled — lived only in a GitHub comment until now.
  `tests/installer_surface.rs` guards the surface statically: no absolute
  foreign path or `Program Files` reference anywhere in `niubash.iss`, every
  `[Files]`/`[InstallDelete]`/`[UninstallDelete]`/`[Icons]`/`[Run]` surface
  confined to `{app}` and Inno-managed shell folders, `HKCU\Environment\Path`
  the only registry value written, and the delete sweeps matched against the
  sanctioned retired-shape globs only. `scripts/installer-slot-probe.py` is
  the runtime leg: a pure user-directory sandbox replays the executable
  installer steps (`--relink-shells`, app-tree teardown) across a full
  install/uninstall cycle and asserts the decoy foreign slot — and the real
  Git Bash slot, read-only — byte-identical at baseline, after install and
  after uninstall, with no face name left outside the removed tree. The same
  probe records the niubash#377 quantification: forwarder faces cost one
  extra process per `-c` call (chain evidence: 2 processes below the probe,
  min ~36 ms) and collapse to the payload itself as hardlinks (1 process,
  min ~19 ms, same file as `niu.exe`).

- **The factory prompt's git segment runs the user's own git** instead of the
  in-process libgit2 dependency: one bounded
  `git status --porcelain=v2 --branch` inside a repository, and **no spawn at
  all** outside one (a few `stat`s, where `__git_ps1` forks anyway). Fidelity
  is the reason to switch — sparse checkouts, linked worktrees, submodules and
  `safe.directory` refusals are answered by the git the user actually runs
  (libgit2 silently dropped the branch segment on old-owner trees once) — and
  the cost is unchanged in practice: measured p50 26 ms / max 37 ms per prompt
  inside a repository, i.e. the bare git call and nothing more, against Git
  Bash's `__git_ps1` 100-300 ms (three to five forks). Isolation rules, one
  per incident: never through a shell (a missing git is an OS error, not
  `command not found` noise), stdout captured and stderr discarded (git
  warnings never reach the screen), `--no-optional-locks` +
  `GIT_OPTIONAL_LOCKS=0` (never takes `index.lock`), `-c core.fsmonitor=false`
  (never starts an fsmonitor daemon from a prompt), a 2 s timeout with the
  child killed, and no `CREATE_NO_WINDOW` (measured +28 ms per call: it makes
  Windows allocate a hidden console for every prompt; a console child inherits
  ours and shows no window). This also drops the `git2`/libgit2 dependency:
  ~1.28 MB of the binary and 323 locked packages (libgit2-sys, url/idna/ICU,
  cc, pkg-config, vcpkg, ...). Both niubash#145 startup invariants still pass
  (`legacy_niu_plugins_rc_reaches_prompt`,
  `legacy_framework_rc_reaches_prompt`): nothing is waited on before the first
  paint, and the prompt-scoped child is reaped by the prompt itself. A
  brand-new repository (unborn HEAD) now shows its branch, where libgit2's
  `head()` errored and the segment vanished.


### Fixed

- **A DSR-silent terminal no longer kills the shell, and TAB completion
  commits** (unixwin/niubash#373, the #355 follow-up). Interactive startup
  used to anchor the prompt with a `ESC[6n` cursor-position query and ABORT
  the line editor when it timed out: on a terminal that never answers
  (dumb terminals, `script(1)` recordings, some serial/tmux links) every
  startup paid a ~2.6 s stall, printed
  "niubash: line editor error: The cursor position could not be read within
  a normal duration", and exited before painting any prompt. The editor
  (reedline 0.50 -> 0.52) now treats the query as advisory — the prompt
  paints without a reply and later cycles stay interactive; Windows/ConPTY
  is unaffected either way (conhost withholds the client at session attach
  until the terminal answers, a terminal-side contract below niu, pinned by
  the new `tests/issue373.rs` harness). File completion now commits
  like command completion: `cat uniqfi<TAB>` splices the completed word
  into the line (the unique match commits outright, the ambiguous set
  commits its common prefix and still shows the grid) instead of leaving
  the suffix as a menu preview that Enter executed unexpanded — and Enter
  submits the line as typed (GNU accept-line), dismissing the grid, where
  reedline 0.52's menu would have consumed the first Enter as an
  acceptance. The dismissal rides only the completion TAB window: a
  Ctrl+R history search's accepting Enter stays the bare event reedline's
  search handler implements (a whole `Multiple` batch is dropped there —
  unconditionally wrapping it turned the search's Enter into a dead key). Measured TTP on the ConPTY harness is unchanged (niu 3.14 s
  vs GNU bash 3.26 s median, transport-floor dominated; internal startup
  ~70-140 ms — `scripts/perf/ttp-bench.py`).

- **dirname/basename**: slash-shaped operands stay verbatim — `dirname /a/b`
  reports `/a` instead of a drive spelling such as `A:`; the slash-drive
  argument mask now covers the filesystem-blind string tools
  (unixwin/niubash#346).

### Changed

- **Windows path contract**: the bundled WinuxCmd coreutils own the shell tree —
  they receive the shell's POSIX spellings verbatim and resolve them in-child —
  while every other child, native or a foreign POSIX runtime such as MSYS2 or
  Cygwin, always receives native Windows paths, with no per-child probe and no
  `MSYS_NO_PATHCONV`-style switch (unixwin/niubash#346). This rides the
  WinuxCmd shell-tree layer, so the bundled WinuxCmd pin must be bumped in the
  same release.
- **Windows releases bundle niu-git 2.55.0.5** (`scripts/release/preinstall.json`).
  niu-git moves to `layout: flat`: the payload lands under `opt\niugit` and
  `mingw64\bin\git.exe` is mapped straight onto `usr\bin\git.exe`, so the `git`
  on PATH *is* the git binary — no winuxcmd dispatch hop, no launcher, no MSYS
  runtime and no perl in the process tree. `git --version` drops from 72–83 ms
  to ~14 ms (Git Bash: 45–103 ms) and `git status` to ~23 ms. Needs WinuxCmd
  1.1.9 or newer (the POSIX-unlink hardlink break, unixwin/WinuxCmd#1183);
  the release pin already carries 1.1.10.

### Fixed

- **`(A;B);)` is a syntax error on every non-interactive route, like GNU** (unixwin/niubash#431). `niu -c '(printf a; printf b);)'` ran the subshell and exited 0 — and so did script files, `--encoded-command`, and piped/redirected stdin — where GNU bash 5.3.0 rejects the shape in every mode (`syntax error near unexpected token ')'`, rc 2, subshell unexecuted; the earlier "msys GNU tolerates the tail" record came from the `winuxcmd/usr/bin/bash.exe` launcher shim echoing niu back — rubash#852). The host fast path now parses with the engine's stray-close guard armed (`ParseLoopOptions::stray_close_is_error`, the same predicate rubash's readers arm) and hands a tripped parse to the engine's group-wise driver, so the diagnostic (verbatim offending-line echo, `$0: -c: line N:` attribution, rc 2) matches GNU byte-for-byte; the no-flag stdin route also binds the argv[0] diagnostic name and keeps one physical line counter across gathered batches like `bash -s`. Pinned in `tests/issue431_stray_close_paren.rs` and `tests/interactive.rs::continuation_subshell_stray_close_is_syntax_error`.
- **Bundled gawk's CRLF output gets a byte-level release gate** (unixwin/niubash#467). The `opt\gawk` payload is a text-mode mingw build — its CRT rewrote every `\n` as `\r\n` on write and folded `\r\n` into `\n` on read, while every other bundled applet does binary stdio, so `awk '{print}' > f` landed CRLF files and `ORS="\r\n"` doubled the CR. The fix ships in the bundled WinuxCmd forwarder (winuxcmd.exe / winuxcmd-shim.exe splice `-v BINMODE=3` ahead of the forwarded argv for `awk`/`gawk` — the build ignores the `BINMODE` environment variable, the `-v` assignment is its only lever, and a caller's own `-v BINMODE=...` still wins by landing later); the pre-install manifest now records why `layout: shim` is load-bearing. The staged-release verify gains raw-byte probes — the packaged `niu.exe` redirects the awk/gawk pipelines to a file and the bytes are compared exactly (`a\nb\n`, and `a\r\nb\r\n` for the explicit-ORS pass-through) — because a PowerShell native-command capture strips `\r` and string equality could never see the regression (that blindness is how the text-mode build shipped). A bundled forwarder without the adapter fails the x64 gate; flipping `winuxcmd-pin.json` to a release carrying the adapter is the same change window.

## [1.4.0] - 2026-10-10

### Changed

- **The factory default prompt on Windows is now the Git Bash skeleton**
  (owner adjudication 2026-10-12, #292): green `user@host`, yellow
  POSIX-persona cwd, cyan branch (+ dirty `*`) and `$ ` — the MINGW64
  environment segment is deliberately omitted (that is MinGW identity,
  not this shell's) and there is no static product tag. The branch/dirty
  segment reads the repository through git2 (owner-approved library
  dependency, Windows-target-only compile) instead of a git child
  process, so the prompt path never spawns (the niubash#145 red line):
  measured cold 39 ms, warm p50 2.5 ms / p90 3.7 ms per prompt against
  Git Bash's per-prompt `__git_ps1` fork (~100-300 ms). Claims still
  win: a PS1/theme claim displaces the floor per prompt, and `set -o
  vi` keeps its live editing-mode indicator (niubash#184). The skeleton
  follows Git Bash's own layout after live owner passes on 2026-10-14:
  two rows (info row, then the `$ ` input line on its own row) and the
  branch in MINGW64's parentheses — `(master*)`, dirty star inside.
- Default TAB completion style is now the multi-per-row grid
  (`CompletionStyle::Column`, owner rulings 2026-10-12 + 2026-10-14):
  every TAB shows the candidates at once, several per row — the look of a
  GNU-style printed listing without the silent first TAB (the literal GNU
  readline shape shipped briefly as the default and read as "dead" at the
  prompt when TAB #1 had nothing to complete; live owner feedback flipped
  it). The literal GNU shape remains an opt-in
  (`NIU_COMPLETION_STYLE=gnu` or `niu config set completion.style gnu`):
  TAB completes the longest common prefix (a lone candidate completes
  outright), and a second TAB on an unchanged line prints the candidates
  as width-adaptive columns next to the prompt and repaints it —
  implemented as a reedline `Menu` (`completion::gnu_menu`) so it renders
  through the normal repaint pipeline. The Ide popup stays opt-in
  (`ide`, also accepted: `smart`, `popup`); precedence is env > config
  file > default, and `niu doctor` shows the effective style with the
  switch commands. The setup wizard gained a completion style step
  (current value pre-highlighted) on first run and on `niu setup`
  re-runs.

### Added

- **Windows Terminal-form desktop/Start-menu shortcuts on Windows 10**
  (niubash#211, #290): Win10 has no default-terminal setting, so
  direct-exe shortcuts used to be hosted by conhost. When a `wt.exe`
  resolves (WindowsApps alias -> PATH -> App Paths registry) and the
  Niubash WT profile is registered, generated shortcuts take the
  `wt.exe -p Niubash` form; without WT the conhost baseline stays
  unchanged. `niu --install-wt-shortcuts` (`--update-only`,
  `--desktop|--start-menu`) is the standalone verb and the installer
  handoff, and it only rewrites links that already exist, so a declined
  desktop icon is never grown.
- **`niu --cwd-state <file>` directory memory for one-shot agent
  invocations** (#272): opt-in, so a chain of one-shot `niu -c` calls
  behaves like one shell — the directory recorded in `<file>` is
  restored before startup (before niu's own startup logic, relative
  script paths, and `source_non_interactive_env`) and the shell's final
  directory is recorded back after every one-shot exit path. Without
  the flag, behavior is byte-for-byte unchanged.
- **`niu --encoded-command <base64>`** (#271): agents and CI callers
  get a quoting-free channel — an RFC 4648 payload (padding optional,
  ASCII whitespace ignored, must decode to valid UTF-8) executes
  through the same one-shot path as `-c`, with the same
  `BASH_ENV`/`NIU_ENV` sourcing and `BASH_EXECUTION_STRING` contract.
  An optional `v1:` prefix is stripped defensively: MSYS path
  conversion rewrites arguments starting with `/`, and a base64 payload
  can start with `/`.
- Doctor timing advice (niubash#201): `niu doctor` now carries an advisory
  row pointing timing/performance-sensitive scripts at the bash 5 builtin
  `$EPOCHREALTIME` / `$EPOCHSECONDS` instead of looping on the external
  `$(date +%s%N)` — each `date` call pays Windows process creation
  (~10-30ms) while the builtin expands in microseconds with the GNU format
  `1699999999.123456`. Regression tests pin the format, second-level
  agreement with `date +%s`, live monotonic updates, and the speed gap.

- Android and OpenHarmony release-CI legs incubating (roadmap niubash#207,
  niubash#247): `build-android` links aarch64-linux-android and
  armv7-linux-androideabi against bionic (NDK r27, API 24) and ships a zip
  per leg with ELF-arch verification in place of an on-runner runtime
  smoke; `build-openharmony` is check-only for now
  (aarch64-unknown-linux-ohos is Tier 2 with host tools — real linking
  needs the OHOS SDK clang wrapper, which has no version-pinned runner
  fetch path yet, so no binary ships from that leg). Both jobs run
  continue-on-error and stay out of the release needs, the same incubation
  shape the musl leg had before it was tightened; the platform-support
  matrix documents both targets.

- Vi editing mode wired end-to-end (niubash#184): `set -o vi` / `set -o
  emacs` now switch the LIVE line editor mid-session, like GNU bash — both
  options route to one readline editing-mode state (bash
  `builtins/set.def:424 set_edit_mode` → `rl_variable_bind("editing-mode")`,
  rebindkeymap applied immediately by readline `bind.c:2092 sv_editmode`).
  The product re-reads the engine option flags at every prompt
  (`Shell::refresh_edit_mode`), rebuilds the Reedline editor on a change,
  and writes the winner back exclusively, so `set -o vi` → `set -o emacs` →
  `set -o vi` round-trips and `set -o` listings stay mutually exclusive the
  way GNU reports them. The rc line `set -o vi` works at startup (the
  editor is built after the rc resolves the mode). Every fresh line starts
  in insert mode even in vi mode (readline.c:1243 "Each line starts in
  insert mode"; reedline keeps ViMode across reads) — a line submitted in
  normal mode no longer leaves the next prompt in normal mode. The floor
  prompt renders a minimal theme-neutral vi-mode indicator (`i ` insert /
  `- ` normal, terminal default color, configurable through
  `prompt_indicators`); a claimed PS1 keeps the theme's own prompt slot
  (GNU bash ships no built-in indicator). ESC enters normal mode through
  reedline's built-in vi parser; normal-mode motions (`k` history recall,
  `dd` line kill) are covered by ConPTY journeys.

- Release-CI pre-install of external packages via wpm (niubash#189): the
  release workflow installs the packages named in
  `scripts/release/preinstall.json` into the staged WinuxCmd root with
  `winuxcmd wpm install <pkg> --root <root> --yes` (wpm ships inside
  winuxcmd.exe — no separate binary) between WinuxCmd staging and packaging,
  so the Windows release zip and installer carry a working GNU awk out of the
  box. First entry: `gawk` (GNU Awk 5.4.1; bash-it plugins and completions
  hard-depend on awk). The manifest also records the shim (`awk.exe`, a
  winuxcmd.exe hardlink that forwards to the `opt/gawk` payload — wpm's gawk
  package registers only `gawk`, and bash-it calls plain `awk`) and the
  owner-ordered exclusions as durable policy: no compression tools
  (bzip2/gzip — users `wpm install` them themselves), `goawk` forbidden
  (gawk only), and `link` forbidden forever (MSVC link.exe collision).
  Installs are fail-open: a failed package (e.g. the arm64 legs while the
  wpm index carries windows-x64 artifacts only) warns in the step summary
  and release notes, and never blocks a release; whatever DID install is
  hard-verified in the staged package (`--version` plus a plugin-shaped
  `awk '{print $1}'` pipeline through the packaged niu.exe) before upload.
  The completions corpus carries real `awk --help`/`gawk --help` transcripts;
  the embedded applet inventory goes 178 → 180 (SKILL.md quickref
  regenerated, golden-gated).
- AI agent skill bundle with `niu skill install` / `niu skill status`
  (niubash#188): the shell ships its own agent-facing guide
  (`skills/niubash/SKILL.md` — identity, capability surface, pitfalls,
  niu-vs-cmd-vs-powershell decision guidance) and installs it into agent
  skill dirs — `--target claude|zcode|cursor|all|<dir>`, default claude;
  `status` compares installed bytes against the embedded bundle and prints
  a sha256 digest; `niu doctor` gained an advisory `agent skill` row. The
  command tables inside the bundle are generated, not hand-written:
  `scripts/generate-skill.py` renders the builtin table (from a captured
  `help -s '*'` transcript), the launcher/plugin verb surface (captured
  `niu --help`), and the 178-applet winuxcmd inventory (from the completion
  assets) into GENERATED-marked regions of `SKILL.md` and
  `references/quickref.md`; `scripts/test-skill-bundle.py` golden-checks
  both the regions and the include_str! manifest (in CI and in the release
  pipeline). Releases now attach `niubash-skill-v*.zip` with the
  WinuxCmd-skill layout (one top-level `niubash/` directory).
- WinuxCmd applet completions are compiled into the shell (niubash#172 L1):
  `grep --col<Tab>` offers `--color`/`--colour` with descriptions, `ls --color`
  offers `always`/`auto`/`never`, `wpm <Tab>` lists its 20 subcommands, and
  every one of the 178 WinuxCmd 1.1.5 applets (including `[`) carries its real
  flag set with no plugin enabled. Definitions are generated from
  `winuxcmd --help` transcripts by `scripts/generate-winuxcmd-completions.py`,
  embedded from `crates/niubash-runtime/assets/completions/winuxcmd/`, and
  load as the base layer: bundle/pack and user-dir TOMLs still override per
  command. Generator fixes en route: bracket command names (`[`), headerless
  help (`top`), multi-alias specs (`pr -F, -f`), `EXIT STATUS:`/footer
  pollution of descriptions, optional-value flags (`--color[=WHEN]`) no longer
  swallow filename completion, path-valued flags decided by spec hints instead
  of description prose, and wpm `Commands:` parsing. Golden coverage:
  `crates/niubash-runtime/tests/winuxcmd_completions.rs` plus
  `scripts/test-winuxcmd-completions.py` (corpus replay must be
  byte-identical).

### Changed

- Architecture correction before release: the carapace-bin completion source
  merged from #243 is removed from the shell core. No shell hardcodes a
  third-party completion provider into its source — the provider belongs in
  the rc data layer, integrating through the standard bash completion
  protocol the shell already speaks (`complete` compspecs, `NIU_COMPDEFS`
  compdef functions, `<cmd>.toml`/`<cmd>.bash` definition files). The native
  completion layers are unchanged (embedded WinuxCmd applet TOMLs, bundle
  definitions, user completion files); `niu setup` grows an opt-in (default
  off) rc snippet for carapace-bin — the same block a bash user writes by
  hand, guarded and a no-op until carapace-bin is installed — and a new
  docs page documents the rc-based contract for any provider that speaks
  the bash completion protocol (carapace is one example). Binary-level tests
  pin the rc route: a `complete -W` compspec and a compdef hook sourced from
  `~/.niubashrc` both serve Tab candidates, and a standard `.bash` completion
  script serves flag candidates through the bash-import channel. Interactive
  Tab resolution of `complete -F`/`-C` hook functions remains an engine
  (rubash) follow-up, documented in the provider page. Zero compatibility
  debt: no 1.4.0 tag ever carried the embedded source, and the `NIU_CARAPACE`
  variable was never a released surface (niubash#243, niubash#187).

- command-not-found no longer advertises package managers by default
  (niubash#249): a typo like `win` prints only the GNU one-liner — the
  unsolicited "package search hints" block naming winget/scoop/choco is
  removed, and third-party managers are never recommended again. The
  recipe/wpm install hints for exact, known tool names (`rg`, `awk`, …)
  stay. An opt-in channel returns a wpm-only suggestion:
  `niu config set command-not-found-hint wpm` (persisted in the new
  `~/.niubash/config.toml`; `NIU_COMMAND_NOT_FOUND_HINT` overrides) — it
  suggests one `wpm search --name '<word>'` line only for
  high-confidence near-misses (dropping the word's last character hits an
  installed command or a known package name), so pure typos stay quiet
  even with the switch on. New `niu config get/list/set` subcommand surface
  manages the file.
- Release binaries are stripped (niubash#228): the release profile strips
  symbols, shrinking the shipped niu.exe.
- The pre-install manifest bundles niugit, ripgrep and fd alongside gawk
  (niubash#230), and the setup wizard detects release-bundled components as
  already-present instead of re-offering them (niubash#231).
- Documentation passes: a "Calling PowerShell from niu" page joins the
  platform docs (when to reach for powershell.exe from niu, and how)
  with a README style pass (niubash#203, niubash#253); the READMEs were
  then restructured evidence-first around quick answers and the
  five-platform identity (niubash#255); the Platform Support page
  documents the status matrix, what the HarmonyOS bash actually is, and
  HNP packaging (#225, #226, #227); and the MSYS labels (`uname` ->
  `MSYS_NT`, `OSTYPE=msys`, `MACHTYPE` `x86_64-pc-msys`) are documented
  as what they are — a compatibility persona of a pure native Windows
  build with no msys-2.0.dll/cygwin1.dll, with the measured behavioral
  boundary (niubash#248).
- One-shot `niu -c` invocations take a fast path that skips REPL-only
  initialization (prompt/theme backend, completion directory scan,
  completion-state snapshot); the history provider and managed aliases
  keep their `-c` contract, and `bash -i -c` still sources the rc.
  Measured 13.9 -> 12.1 ms in-process startup for `niu -c 'echo hello'`
  with a 29-case behavior battery at zero diff (#270).
- REPL-only commands (`self-update`, `update-niubash`, `logout`) are
  highlighted as builtins instead of unknown-token red (#281).
- The easter eggs are pruned to snake and about; the about logo is
  pyfiglet-exact (#264).
- The declared MSRV is 1.95 — reedline 0.50 in the lock requires it
  (#267).
- Installer disclosure (niubash#313): the "Create a desktop shortcut"
  task is new in 1.4.x and default-checked. Silent installs and upgrades
  (`/SILENT`, `/VERYSILENT`) never show the task page, and Inno has no
  saved selection for a new task name — so a silent upgrade of a 1.3.x
  machine runs the task with its default state and grows a desktop icon
  (rewritten in place to the Windows Terminal form by the shortcut step)
  without ever asking. Owner ruling: the behavior is intended; it is
  disclosed here and in the installer source. Interactive installs keep
  remembering an unchecked choice.

### Fixed

- **The self-update installer is SHA256-verified, fail-closed, and the
  release ecosystem is pinned** (niubash#311, niubash#312, PR #318):
  every Windows setup.exe ships with a coreutils-style `.sha256` sidecar
  (plus an aggregate `SHA256SUMS` for the whole release — both are
  release assets, and the same digests feed the winget manifest).
  `niu self-update` downloads the sidecar published beside the same
  asset it fetched, hashes the installer, and aborts before launch on
  any mismatch — or when the sidecar cannot be fetched at all (the
  error points at the manual-download path). Release builds no longer
  taste the live ecosystem: workflow dispatches require a pinned
  WinuxCmd tag plus per-arch zip digests, tag pushes read the committed
  `scripts/release/winuxcmd-pin.json`, and the wpm pre-install manifest
  pins every package's artifact SHA-256 per arch (pin == index ==
  shipped bytes; a pin failure keeps the loud fail-open warn-and-ship
  path).
- Plugin verbs can no longer hang forever on a dead origin, and a failed
  add no longer strands its spec declaration (niubash#293, niubash#294,
  niubash#295, niubash#296). Every git transport a plugin verb spawns —
  `niu plugin add`/`sync`/`update`/`restore`/`distro import` — now runs
  under a 120 s wall-clock budget (`--timeout <seconds>` on `add`/`sync`,
  or `NIU_PLUGIN_FETCH_TIMEOUT_MS`, overrides it per run; the rc
  `--bootstrap` form keeps its tighter 3 s): at the budget the clone's
  whole process tree is killed (no more orphan `git.exe` helpers after a
  hand-killed niu) and the failure row names the timed-out origin. The
  spawned git also carries the user's proxy environment explicitly
  (`HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY`, each mirrored to
  its case-variant, so an uppercase-only environment still reaches git's
  case-sensitive transport). A sync stays per-source isolated: one
  stalled origin costs its own budget and a failed row while the rest of
  the spec reconciles. A declaration whose install just failed joins the
  stranded population, so `niu plugin sync --prune` removes it in the
  same run — and `niu plugin add` of a dead origin rolls its declaration
  back on the bounded failure, so the target can be re-added instead of
  deadlocking on "already declared".

- The Windows paste-chunk detection no longer swallows the first Enter of
  every line (niubash#284, a niubash#252 regression): whether a bare Enter
  submits or opens a paste chunk is now decided structurally — by the LF
  partner (`Enter + CONTROL`) queued right behind it, the console's shape
  for a machine-fed CRLF line break — not by the 50 ms arrival-burst
  heuristic. "Text + Enter" in one burst (fast human typing, ConPTY test
  drivers, expect scripts, `send-keys`) submits on the first Enter again;
  multi-line pastes keep buffering quietly until the user's own Enter
  (0 mid-paste executions, niubash#202 contract unchanged). A typeahead
  line queued behind a submitting Enter is pushed back and lands, in
  order, at the next prompt. The TypeaheadGuard no longer double-injects
  swept console input records: the sweeper's self-heal path drains the
  buffer before writing it back, so a record can no longer be reinjected
  twice (once by the self-heal, once by disarm; niubash#287).
- A paste burst's trailing bare Enter now submits the buffered chunk
  (niubash#308): a paste followed immediately by Enter — one write, the
  Enter inside the burst window — used to be swallowed, and a second
  Enter was needed. The same LF-partner lookahead that decides the
  chunk-open Enter now runs in the pending branch: silence (or any
  other queued key) behind the CR proves the chunk complete, and the
  chunk flushes and submits in one batch on the first Enter.
  Execution-level paste regressions pin empty-line fidelity across
  every shape (niubash#291, PR #317).
- The plugin trust gate now runs at runtime, not only at trust time
  (niubash#297): `niu plugin sync` and `plugin list` re-verify every
  trusted source against its lockfile pin, so a tree modified after
  `trust` reads `drift` (never a lying `unchanged`) and
  `degraded=true` with the drift reason in `--json` — the tree stops
  contributing assets until it is repaired. sync records drifted ids in
  `sources/drifted-sources.txt`, and the managed rc blocks carry a static
  guard that prints the warning at every startup (the rc stays static
  text; no hashing in it). `plugin restore` wording is honest about
  rebuilding from the pin ("rebuilt from the lockfile pin", not "tree
  matches"). bash-it's `enabled/` entries — the one legitimate tree
  mutation `niu plugin enable` itself makes — are transparent to the pin,
  while an injected `enabled/` file with unpinned content still trips
  both `verify` and `trust`.
- `niu plugin mirror set http://…` is refused without an explicit
  `--allow-plaintext` confirm (niubash#298): a plaintext mirror ships
  every git fetch unencrypted and unauthenticated, and the confirm still
  prints the standing warning. The first fetch of an unpinned non-catalog
  origin is now flagged for what it is: `plugin add` prints a
  "NOT integrity-verified" warning and the record carries
  `first_fetch_verified: false` (visible in `plugin list --json`); a
  `--checksum` pin or a curated catalog origin marks it verified.
- `niu plugin clean` reports every orphan directory under `sources/` the
  registry does not claim (niubash#299) instead of silently keeping
  manager-shaped ones and ignoring the rest; `--orphans` is the explicit
  confirm that deletes them.
- `niu plugin add <path>` positional form is first-class (niubash#300):
  a bare local directory snapshot-copies in, a single `.sh`/`.bash`
  file becomes its own one-file wild source, a nonexistent path fails
  honestly (instead of a baffling `git clone` status 128 after the
  trust-boundary banner), and the help documents the path and
  `file://` forms.

- **Managed rc-block rewrites converge and self-repair** (niubash#324,
  PR #325): an rc already carrying "user lines moved out" debris (the
  issue's six-section fixture — five dangling `if`s, a syntax error on
  every startup) stayed corrupt through every sync/enable/disable
  forever, because the rewrite paths never inspected the user area.
  Every plugin verb now normalizes the migrated sections first: a
  section whose every non-comment line is guard-shaped goes entirely, a
  section mixing genuine user lines with debris is kept verbatim
  (whole-section edits only — niubash#176 keeps user lines
  sacrosanct), and a re-fed user line merges into the pile instead of
  appending a duplicate section per verb. Even the startup
  `niu plugin sync --bootstrap` repairs the file — no one-shot cleanup
  command needed.
- **Completion covers the commands people actually type** (niubash#305,
  #306, #307, PR #319): git, tar, curl, ssh, docker, kubectl and npm
  are not WinuxCmd applets and completed as bare filenames — a second
  compiled base layer now ships hand-curated static subcommand/flag
  tables for them (no external completion script is ever executed).
  `NIU_COMPDEFS` lines that resolve to an existing directory now
  declare completion-definition directories (`<cmd>.toml` on the
  embedded applet schema, `<cmd>.bash` statically parsed by the
  bash-import reader — file bodies are never executed); previously the
  declared surface had no user-reachable entry point and zero effect in
  every mode. And `--completion-probe` sources `NIU_ENV` (`BASH_ENV`
  fallback) before the interactive rc, so compspecs declared in an
  agent init file reach the probe.
- **`niu -s` reads stdin scripts on demand** (rubash#586 host half,
  PR #314): the whole stdin used to be slurped with `read_to_string`
  before execution, so a `$(exec cat)`/`read`/top-level `exec cat`
  consumer hit EOF immediately and the unconsumed tail came back as
  code. The host now drives the engine's on-demand reader (the rubash
  #593 library surface): the consumer sees exactly the bytes after the
  last submitted command, and the repro is byte-verified against WSL
  GNU bash 5.3.0.
- **The bash/rbash invocation faces report their name** (niubash#321,
  PR #323): `bash -c 'echo $0'` printed `niu` — the shim's
  `NIU_INVOKED_AS` forward now carries all four faces
  (sh/ash/bash/rbash, login marker and `.exe` stripping preserved) and
  POSIX mode stays keyed off the face; the d3 setup-rerun journey
  answers the new TAB-completion wizard step (niubash#322).
- The `niu plugin` help and doc examples no longer point at a dead repo:
  the wild-GitHub-plugin example was `niu plugin add
  rcrowley/bash-preexec`, but rcrowley/bash-preexec 404s (the project
  moved to rcaloras/bash-preexec years ago), so a copy-paste of the
  example failed. Every live reference — help text, the plugins.toml spec
  example, the plugins guide, the wild-source fixture — now names
  rcaloras/bash-preexec, and the example repo was verified cloneable
  (niubash#301).

- The config subsystem honors `HOME` isolation (niubash#302): every niu
  subsystem — config.toml, mirrors.toml, the plugin rc, doctor, prompt,
  the setup wizard — resolves the home directory through one shared
  resolver in which an explicit `HOME` environment override wins on every
  platform. Before, the Windows resolver ranked `USERPROFILE` first, so a
  HOME-only sandbox (tests, CI, portable setups) silently read and wrote
  the real profile. A default Windows session (`HOME` unset) resolves
  exactly where it always has, and the classic Unix `HOME`-first
  convention is unchanged.

- The freshly-rendered mirrors.toml template no longer recommends
  hub.fastgit.xyz (dead since ~2021; niubash#303). The second community
  example is now ghproxy.net, verified live at edit time with a real
  `git clone` through the `insteadOf` rewrite (gitclone.com, the first
  example, re-verified the same way). The examples stay comments marked
  UNSUPPORTED — they may die at any moment and are the user's to verify.

- `.niubashrc` rewrites are atomic (niubash#304): every managed-block
  rewrite (`niu plugin enable/disable/sync`) and the setup wizard's rc
  write now stage the new content in a same-directory temp file and swap
  it in with a single replace-rename (`MoveFileExW
  MOVEFILE_REPLACE_EXISTING` on Windows, `rename(2)` elsewhere). The
  former truncate-in-place write and the wizard's former
  remove-then-rename left windows where a concurrent reader — a pipe or
  `sed` run against the rc — saw the file empty, partial, or briefly
  absent; a reader now always observes a complete old or new generation.

- The installer cleans up after itself (niubash#313). Uninstall now also
  removes the `{app}\winuxcmd\bin` entry from the user `PATH` — the
  install-time `wpm links rebuild` adds it, but the uninstaller only ever
  removed `{app}`, leaving a dangling PATH entry after every uninstall.
  The installer gained `[InstallDelete]`/`[UninstallDelete]` sweeps for
  files earlier builds shipped and the current payload retired (the
  reverted dual-name build's `niubash.exe`, stray self-update
  `niu.exe.old-*` backups), which previously survived upgrade AND
  uninstall on real machines.

- A closed stdout pipe terminates niu (niubash#245, PR #256): when the
  shell's own stdout is the pipe whose reader went away, non-interactive
  executions interpose a kernel pipe + pump thread and exit with one
  diagnostic and status 1 — previously, with no SIGPIPE on Windows,
  every later write failed again and an unbounded builtin loop printed
  one diagnostic per iteration forever (`niu -c 'yes' | head -c 1`
  never exited). The contract is pinned across every non-interactive
  execution shape — script-file producer, stdin-script producer,
  external child as the producer, and stderr gone too (niubash#286,
  PR #289).
- The history provider writes the final list back to `HISTFILE` at
  session end (niubash#265) — the missing exit half of the
  niubash#182/#250 GNU `-c` semantics: after a one-shot
  `niu -c "history -c"` the host file holds the empty list, like GNU
  bash's `exit_shell()` write. The rc-0 one-shot path returns from main
  instead of `process::exit` so the write runs.
- `sh -c` enters POSIX mode with `$0 = sh` (niubash#262): the installed
  `sh.exe` is the same forwarder binary as `bash.exe`, so Windows
  argv[0] could not carry the invocation contract to niu. The shim
  forwards its argv[0] stem as `NIU_INVOKED_AS` (actively scrubbed
  under every other name, so a nested `sh -c 'niu ...'` keeps its own
  face).
- Path-form expectations align with the engine's POSIX persona
  (niubash#283): the host cwd sync no longer re-stamps the executor PWD
  into native drive form, so the rendered form no longer depends on
  which route executed the command (simple sequences vs compound lines
  and command substitutions).

- Plain-text `--help` and a non-tty-safe banner (niubash#229), plus a warning
  when a hand-written rc would clobber the wizard theme.
- PROMPT_COMMAND exit jump now ends the session instead of returning to the
  prompt (niubash#232).
- PS0 expansion writes to stderr, matching GNU bash (eval.c:176) (niubash#233).
- Bracketed paste enabled with GNU bash multiline-paste semantics: pasted
  newlines no longer execute mid-paste (niubash#234).
- The bash-it theme history interlock (niubash#182) is closed: themes
  running `history -a && history -c && history -r` from PROMPT_COMMAND
  doubled the history file every prompt and wedged the session. `history
  -a` appends only what the file lacks (bashhist.c), `history -r` rebuilds
  the reader from disk without re-appending reedline's unsynced buffer,
  and `history -c` clears the in-memory list only — the backing file is
  snapshotted and restored instead of deleted
  (builtins/history.def:185). A ConPTY regression drives 12 prompt cycles
  of the exact theme pattern and asserts every command lands in the file
  exactly once (niubash#250).
- An unparsable plugin source registry is never silently rewritten
  (niubash#178): registry.toml bytes that fail to parse are snapshotted to
  `registry.toml.corrupt` and warned on stderr, and every registry write
  is refused — sync/install fail loudly instead of dropping the persisted
  pin commit, checksum and trust state riding in the file; install refuses
  before any fetch, and `niu doctor` names the parse error's line and the
  sidecar instead of "none installed" (niubash#246).
- Multiline paste on the Windows console no longer executes line-by-line
  (niubash#202): Windows gets no bracketed paste (ConPTY swallows the
  ?2004 markers), so a terminal paste arrived as per-line key events that
  executed as they came — a 200-line paste self-executed 186 times
  (176 KB of output, 15-17 s). A PSReadLine-style arrival-burst heuristic
  (`PasteChunkEditMode`, 50 ms inter-key gap, cfg(windows) only — Unix
  keeps the real bracketed paste above) buffers the burst, including bare
  Enters and the console's CRLF Enter pairing, and commits it as one
  insertion; the script executes exactly once on the user's Enter
  (paste-phase executions 186 -> 0; the issue's 257-line payload drops
  from >300 s timeout to ~4.6 s) (niubash#252).
- The setup wizard audit P2 batch (niubash#179): a fresh install journals
  rc_created so the finish screen's undo receipt covers the generated rc
  itself; duplicate undo receipts for one source dedupe; Ctrl-C at the
  niu-git question prints the same cancelled note as every other question;
  `niu plugin discover` enumerates the theme layer of trusted sources; zh
  translation keys realign with the runtime literals (now guarded by the
  vocab test); a gutted trusted source makes `niu plugin list`/`discover`
  name the repair verb (`niu plugin source verify <id>`) instead of
  silently showing no assets (niubash#239).
- `niu doctor` on Linux/macOS reports the Windows-only checks (winuxcmd
  core, command links, bash/sh shims) as n/a rows outside the critical
  tally instead of "1/3 critical checks passed"; the header names the
  platform and the terminal row falls back to `TERM` (niubash#194)
  (niubash#242).
- A raw ESC byte as the first character inside single quotes in a script
  file is preserved (niubash#200): the word-initial data ESC was claimed
  by the lexer's QUOTED_WORD_PREFIX marker and silently dropped; the
  engine lexer now raw-byte tags it (rubash#474) and integration tests
  pin the byte at the niu boundary across single/double-quoted, unquoted,
  ANSI-C and piped-stdin spellings (niubash#235).

### Engine (rubash 1.4.0)

- Login shells: `-l`/`--login` (and an argv[0] starting with `-`) run the
  GNU startup chain on Unix — `/etc/profile`, then the first of
  `~/.bash_profile` | `~/.bash_login` | `~/.profile`; non-login
  interactive shells source `/etc/bash.bashrc` + `~/.bashrc`;
  `--noprofile`/`--norc` suppress the corresponding branch. Windows
  accepts the flags with unchanged behavior (no login concept).
- A bare-console `rubash` gets real readline line editing: a raw-mode
  console reader translates keystrokes through the engine's editor
  dispatcher — history recall (arrows, C-p/C-n), in-line cursor motion,
  C-r reverse search, per-keystroke redraw — and restores cooked mode for
  child processes.
- Command-substitution exit status is published to `$?` during expansion,
  like GNU: `echo "$(cd nope && pwd)" $?` reports 1,
  `false; echo "$(true) $?"` reports 0 (rubash#485).
- Compatibility families zeroed against the GNU 5.3.0 true-baseline
  suites: dbg-support (635→0), restricted rbash (194→0), invocation
  (BASH_ARGV0, login argv0, long-option table — 14→0), trap, function
  definition (POSIX funcname rules, AST printer), multi-operand
  `complete`; the 83-suite diff drops 3427 → 2072 lines (−40%), 32
  suites now byte-equal.
- History: `history -d start-end` range deletion (a GNU 5.3 feature), fc
  -s re-execution semantics; globstar `**` fixes (non-adjacent patterns,
  zero-depth trailing slashes); array/assoc compound-assignment quoting
  fixes; same-line `#` comments after `f() {`; unquoted variable command
  words split by IFS in pipelines; `eval` inside command substitution
  re-parses like GNU; `set -u` arithmetic no longer flags assigned
  variables as unbound.
- Signal numbers unified on the Linux table (USR1=10, CHLD=17, RTMIN=34)
  so `kill -l`/`trap -l` match GNU 5.3.0; aarch64/armv7 Android compile
  against bionic; SIGPIPE no longer aborts the interpreter (`yes | head`
  streams like GNU).
- Utilities: GNU cp port (-r/-n/-i/-v/-p/-u/-t with buffered stderr),
  /bin/echo and /usr/bin/echo route to the buffered builtin channel;
  printf/ANSI-C `\u` partial-read and hex-width rules; locale subsystem
  (setlocale warnings, MB_STRLEN).

## [1.3.4] - 2026-10-05

### Fixed (engine rubash 1.3.4)

- Theme choices survive `source` and new terminals: the wizard/gallery pick
  is pinned into the spec, sync reconciles same-name claims toward the rc
  (the "activated/deactivated" revert is gone), and the defaults-as-floor
  never resurrects a framework over a claimed theme.
- Multi-line theme prompts place the editing cursor at end-of-input-line:
  the theme's right-align cursor surgery is served through the line
  editor's own right-prompt channel (escape-excluded width math).
- A 6-second first prompt after picking `full` is gone: startup fetches run
  under a hard 3s budget (kill-on-close job object, credential-prompt
  guard) and memoize; in-sync machines never fetch at all.
- `tr -d/-s/-c` in pipelines actually runs (the translate fast path had
  read `-d` as a character set); `for f in "${arr[@]:-}"` yields
  per-element words; `/usr/bin/seq`-style file operands resolve through
  `.exe` existence (directory forms unchanged); parser throughput +13%
  (nvm -n 18.0x -> 15.6x vs GNU).

### Fixed (product, found by the audit lanes and the golden journey)

- add/rc audit P2 batch (niubash#176): `niu plugin add -h` is a usage
  request (was an unknown-option error); the plugin usage and the add usage
  document `--url`; a local directory named like a catalog id
  (`oh-my-bash/`) is adopted as a local tree instead of being hijacked to
  the catalog's GitHub origin; cwd-relative path targets are stored
  cwd-independent (normalized absolute, forward slashes) in the spec; the
  already-declared refusal points stranded declarations at
  `niu plugin sync --prune` instead of a remove that would fail; user lines
  hand-written inside a managed rc block are migrated out with a warning
  instead of being silently dropped; a CRLF rc keeps its line endings
  through block rewrites; hand-edited near-marker blocks are repaired
  instead of duplicated; every plugin verb that rewrites the rc backs it up
  first and the backup set is pruned to the 10 newest.
- `plugin add --checksum` is honored through add and adopt; failed adds
  roll back their spec entry and legacy strands are pruned; enable/disable
  surface rc-write failures and collapse duplicate managed blocks.
- The wizard rc's bootstrap line references the running binary
  (`${NIU_SHELL:-niu}`), so a stale niu on PATH can no longer hijack it;
  doctor warns when the PATH-resolved niu differs from the running exe.
- `--help`/usage/docs truth pass (ui/recipe/distro/mirror/source/rollback
  verbs listed; sync --adopt documented; WinHttp 12175 gets a reason).

### Added

- Theme gallery live prompt preview (isolated render, 1.5s bound) in the
  wizard and `niu plugin ui` (verb now registered) - highlight moves, the
  pane below renders that theme's actual prompt.
- Collections install curated independent recipes (complete-alias,
  fzf-git.sh, bash-sensible, git-flow-completion - upstream-audited).
- 178/178 winuxcmd applet completions embedded (zero-config Tab completion
  for every bundled command; user TOMLs keep highest priority).
- The golden user journey is a required release gate: install -> wizard ->
  trust -> theme -> new terminals -> daily battery, plus persistence,
  wizard-rerun, spec hand-editing and remove-active-source phases
  (`--phases all`), all driven from the tag's own fresh build.
- Per-asset timing budgets (437 themes/plugins/completions, zero
  sampling) wired as a second release gate; budgets ratchet down only.

## [1.3.3] - 2026-10-04

### Fixed (engine rubash 1.3.3)

- Keystrokes typed while the prompt rebuilds are no longer eaten: external
  children spawned by PROMPT_COMMAND (the theme clock's date/git/awk probes)
  used to consume the first pending console input record - a human typing
  right after a prompt refresh lost their first key (`echo` -> `cho`). A
  typeahead guard now sweeps pending input during prompt machinery and
  reinjects it verbatim before the editor reads.
- `tr` with options in a pipeline actually runs (`tr -d b` was silently
  passed through - the translate fast path had read `-d` as a character
  set).
- `for f in "${arr[@]:-}"` yields per-element words like GNU (bash-it
  theme loading works).
- Parser throughput: nvm -n ~13% faster (backslash-continuation scan
  admission gate + binary-search line stamping), GNU ratio 18.0x -> 15.6x.

## [1.3.2] - 2026-10-04

### First three-platform release (engine rubash 1.3.2)

- **Linux (x86_64 + aarch64, glibc) and macOS (arm64 + x86_64) portable
  tarballs** join the Windows packages. Every platform's build job
  smoke-runs the binary it ships, on that OS, before any artifact is
  attached. Windows asset names and the self-update link logic are
  unchanged.

### Engine fixes

- Alias expansion no longer leaks into compound array assignments OR
  `[[ ]]` conditional / extglob pattern words - the oh-my-bash +
  bash-completion interactive corruption family is closed (the
  `bash_completion: line 1376` syntax error).
- Single-quoted words keep their integrity through pipeline and
  command-substitution stages (three stacked root causes fixed).
- `cat` applies all formatting options when reading a pipe (`cat -n`
  in a pipeline numbers lines).
- `cd` reaches external children (interactive reader no longer restores
  the process cwd per line).
- `-c` subshell fatal expansions exit 1 like GNU, not 127.
- `\u` renders the OS account name, env-independent.
- Virtual system-root arguments (`/usr/bin`, `/etc`, `/tmp`, ...) are
  resolved through the root map for every child class.
- Console-attached bare rubash renders the real PS1/PS2 channel instead
  of a placeholder REPL (GNU interactivity rule, error survival, exit
  codes).

### Product fixes (found by the new golden user journey gate)

- Same-name themes (powerline-multiline exists in both frameworks) now
  route to the owning framework's native loader and actually apply.
- The curated bash-preexec recipe entry matches the real upstream file;
  all curated entries are pinned against upstream roots.
- A failed collection apply journals and reports the failure honestly
  with the retry command.
- The golden user journey (fresh install -> wizard -> trust -> theme ->
  new terminals -> daily battery) now runs as a required release gate.

## [1.3.1] - 2026-10-03

### Fixed

- **Interactive arithmetic corruption** (engine rubash 1.3.1): aliases no
  longer expand inside compound array assignments. Under oh-my-bash, the
  convenience alias `1='cd -'` leaked into the literal
  `OMB_VERSINFO=(1 0 0 0 ...)` and corrupted it to
  `(_omb_directories_cd - 0 ...)`, killing version arithmetic with
  `-: arithmetic syntax error` in interactive sessions (any theme,
  powerbash10k included). GNU parity: assignment words are never
  alias-expanded (parse.y parse_compound_assignment keeps the whole
  `( ... )` in one ASSIGNMENT_WORD token).
- **Plugin sync state machine**: the startup "installed but not declared"
  nag now prints only when a spec exists (imperative mode is silent);
  `niu plugin sync --adopt` declares installed sources with a
  round-trip-stable snapshot (enablement + theme); the setup wizard ends
  spec-managed; `niu plugin add` adopts an already-installed source
  instead of erroring "remove it first".
- **No re-download loop**: the startup bootstrap memoizes failed installs
  (per origin+ref ledger); retries happen only on an explicit
  `niu plugin sync`. The bash-it fingerprint now matches the real
  upstream layout (its `lib/composure.bash` was removed upstream).
- `niu plugin source remove` also drops the spec declaration (no
  resurrection), and duplicate spec declarations of one source merge
  instead of flipping the rc block every sync.

## [1.3.0] - 2026-10-03

### Plugin system - the bash ecosystem, managed

- Declarative spec `~/.niubash/plugins.toml` (single source of truth) +
  `niu plugin sync` reconciliation, startup bootstrap, and a
  commit-pinned lockfile - the lazy.nvim spec/lock conventions.
- Any-plugin generality: `niu plugin add <owner/repo | url | path>`;
  source adapters for oh-my-bash, bash-it, bash-completion and bpkg;
  framework assets enable only through their native loaders (loader
  fidelity - no shims, no bypass paths).
- Recipe index (500+ curated rows across managers/themes/plugins/
  completions/prompts), collections/distros (`niu plugin distro
  import`), menu TUI (`niu plugin ui`), read-only `niu plugin discover`.
- Mirror pipeline: transport-layer git `insteadOf` rewriting
  (`niu plugin mirror set <url>`); lockfiles keep canonical URLs.
- Trust model: tree-checksum verification, explicit `niu plugin trust`,
  and a trust question in the setup wizard.

### Download retraction (BREAKING)

- niu no longer downloads anything except git clones: ureq, flate2,
  tar and zip are gone, the download module is deleted, and the binary
  shrinks ~16%. The smoke suite asserts a zero-network surface.
- Fonts become detection + recommendation (winget nerd-fonts packages,
  scoop, brew, nerdfonts.com).
- Executable tools become package-manager recommendations only: `wpm`
  first on Windows, native managers (apt/dnf/yum/brew) elsewhere.
  The `niu plugin tool` verbs are retired.

### Setup wizard - one-run out-of-box

- Plugin collection page (recommended = oh-my-bash + its default theme
  + completions; full adds the second framework and bash-preexec with
  fzf/starship as suggested installs).
- Post-install theme pick: after a collection applies, the wizard asks
  the trust question and then offers the freshly installed themes - a
  complete look in ONE run, no second `niu setup`.
- defaults-as-floor: enabled external frameworks own the prompt; the
  built-in default is the floor, never an override.

### Engine (rubash 1.3.0) and bundled components

- TMPDIR child boundary fixed: invented defaults stay shell-only and a
  genuinely exported TMPDIR crosses in Windows-native form - Bun-
  compiled TUIs (opencode etc.) launch again.
- Associative kvpair compound assignments, `:` in redirect filenames,
  command-substitution case-depth regions, physical-line diagnostics
  across continuations, and spawn diagnostics preserved verbatim.
- WinuxCmd 1.1.5 bundled: xargs stops its own option parsing at the
  utility name; mv/mktemp/cp/ls accept POSIX drive-form operands; yes
  dies with 141 on a broken pipe like GNU coreutils.

### Fixes / tests

- Unix cross-check compile break in the setup wizard (a baseless
  cfg gate); a one-character zh translation key mismatch.
- Release smoke suite now 17 legs, fully offline, including the
  one-run wizard journey under ConPTY.

## [1.2.5] - 2026-10-02

### Fixes

- **`$(case y in (b|case) ...)` family** (rubash #380, P0): the comsub
  body is a fresh command stream — its first word is at command position
  — and the esac keyword rule looks at the PREVIOUS token, not forward
  evidence. All paren-list keyword patterns now parse and run.

- **posix round-trip stickiness** (rubash #383): the set_posix_mode
  walk is ported — enable arms inherit_errexit and friends, disable
  resets only the two GNU resets (inherit_errexit stays sticky).

- **SIGPIPE-shaped lingerer status** (rubash #382): `yes | head -3;
  echo ${PIPESTATUS[0]}` prints 141 like GNU.

- **Fatal expansion inside $( ) under -c** (niubash #154): reports 1
  (EXECUTION_FAILURE), not 127.

- **Foreign Git Bash PS1 discarded** (niubash #117): a PS1 carrying
  `__git_ps1`/MSYS title escapes is unset before rc — the own theme
  renders, no more command-not-found per prompt.

- **Test suites rebuilt against GNU 5.3.0 probes** (rubash #373:
  88 reds -> 0; #374 first pass 174 -> ~119).

### Performance

- Pipeline-floor round 2 (rubash): __RUBASH_CURRENT_LINE single-writer
  + $_ equal-skip rebind — p-null -4.7%, p-f1 -5.7%.

- Engine bump: niu 1.2.5 builds against rubash 1.2.5 (c2f8e8a6).
## [1.2.4] - 2026-10-01

### Fixes

- **Distributable binaries no longer require VCRedist**: release builds
  now statically link the CRT (`-C target-feature=+crt-static`). niu.exe
  previously imported VCRUNTIME140.dll — not an OS component — so on a
  clean Windows without VCRedist 2015+ the loader failed with
  STATUS_DLL_NOT_FOUND (0xC0000135) before any shell code ran. This was
  the winget validation sandbox failure on winget-pkgs#437563 (and the
  same exit code had masked it behind the portable-tree issue in #150).
  Imports are now OS in-box DLLs only (verified with objdump).
## [1.2.3] - 2026-10-01

### Fixes

- **External-pipeline data loss** (#141, #155, P0): the Windows
  broken-pipe hard-kill window for non-final pipeline members armed
  unconditionally at call time, killing healthy producers mid-stream
  while their consumer was still reading (`seq 200000 | wc -l` returned
  ~17k with rc=0, drift per run). The window now arms only after the
  downstream member is observed to have exited, plus a 100ms natural-exit
  grace (rubash 2c781657). Verified: seq 5000000 x5 full count;
  `yes | head` lingerer termination intact.
- **`$()` children see pre-opened fds 3/4** (rubash #368): the nvm-exec
  `3>&1`/`1>&4` juggle protocol holds; fd-1 dup snapshots escape the
  substitution to the real stdout.
- **`${assoc[*]@A}` keeps the assignment body** (rubash #371);
  **case-pattern keywords stay inert** (rubash #372, issue308 residue
  green).
- Engine bump: niu 1.2.3 builds against rubash 1.2.3 (a6eb8451).
## [1.2.2] - 2026-10-01

### Fixes

- **Pipe data loss on external-to-external pipelines** (#141, #155): the
  external-command stdio planner and the captured-output drain were
  rewritten (rubash #370 family). On 1.2.1, `seq 200000 | wc -l` could
  return ~24k-33k lines (or empty) with rc=0 while the writer took EPIPE;
  compound bodies (`{ seq 200000; } | wc -l`) were unaffected. Engine
  release builds now pass 8/8 at 200000 with empty stderr.
- **bats-core self-suite hang** (rubash #364): an assignment value coming
  from a parameter-expansion result no longer re-executes `<(cmd)` text
  found in the EXPANDED value (GNU subst.c:11358-11381 semantics);
  bats_pipe.bats 155/155 TAP byte-identical to GNU.
- **Adjacent `$((...))` arithmetic substitutions mis-sliced** (rubash
  #376, P0): `echo "$((1+1)):$((2+2))"` now prints `2:4` — the whole-word
  admission uses a real paren-depth span scanner (GNU parse.y:3877
  parse_matched_pair) instead of pairing the first `$((` with the last
  `))`.
- **Quoted compound-assignment elements globbed** (rubash #369):
  `arr=("$x")` with `x='*'` stores the literal `*`; quoting state now
  survives transport to the element glob gate.
- **`exec N>&M` fds honored by external commands** (rubash #370):
  `exec 3>&2; helper >&3` lands on stderr for external children, not
  stdout.
- Engine bump: niu 1.2.2 builds against rubash 1.2.2 (98bc65ba).

## [1.2.1] - 2026-10-01

### Fixes

- **WinGet portable install could not start `niu`**: the manifest shipped the
  release zip with `InstallerType: zip` + `NestedInstallerType: portable`, and
  WinGet's portable shim only copies the single declared `niu.exe` into
  `%LOCALAPPDATA%\Microsoft\WinGet\Links`. The bundled `winuxcmd/usr/bin`
  tree next to the executable is left behind, so `niu.exe` started from the
  Links directory found no WinuxCmd and aborted during startup validation
  (winget-pkgs PR #437563, exit code 0xC0000135). The manifest now installs
  the Inno Setup `-setup.exe`, which lays down the full directory tree and
  registers the PATH entry.
- Engine bump: niu 1.2.1 builds against rubash 1.2.1.

## [1.2.0] - 2026-09-25

### Fixes

- **Drive-letter colons are preserved when niu splits the shell `PATH`**
  (`3c7b4b1`); PATH entries like `C:/tools/bin` no longer get mangled into
  `C` + `/tools/bin` during PATH processing
- GNU-aligned invocation surface for stdin scripts: fd0 handling and `-i`
  history flag match GNU bash behavior (`746b74d`, rubash-side fixes)
- Engine bump: niu 1.1.5 builds against rubash 1.2.0 (published to crates.io),
  which carries the `/dev/stdout` `/dev/stdin` `/dev/null` redirect semantics
  fixes (`8c0dded3`..`a38268f6`) and the gate suite now runs 86/86 green

## [1.1.3] - 2026-09-16

### Fixes

- **`ln -s` with `./` / `../` targets produced links that Explorer could not open**.
  winuxcmd stored the link text verbatim; NT only resolves reparse-point targets
  with backslash separators, so any forward slash (`./bds`, `../x`, `dir/file`)
  failed native resolution with "The filename, directory name, or volume label
  syntax is incorrect" (WinuxCmd #1101, fixed in v1.0.8, niubash #109)
- **`niu -lc 'cmd'` (and any bundled short option containing `-l`/`-i`) failed**
  with `-l: invalid option`; bundled short options now expand correctly
  (`-lc`, `-cl`, `-ilc`, `-ic`) (rubash, PR #112)
- `niu -c -l`, `niu -c` argument handling and `$(type -t)` stdout leak from
  the v1.1.2 issue batch (#106/#107/#108, rubash PR #111)

## [1.1.0] - 2026-09-12

### Highlights

One week of intensive work after v1.0.1: IDE-style completion menu, colorized
plugin CLI, NIU_ENV/BASH_ENV one-shot env files, startup-overhead fix, release
binary size reduction (LTO + panic=abort), and upstream rubash v1.1.0 with the
CTLESC `\x11` leak fix that resolves `NIU_BINDKEYS` parsing.

### Features

- **IDE-style completion menu** with descriptions; humanized plugin CLI views
  (`26610e7`, `a6c1fee`)
- **Colorized plugin CLI**: TTY-gated ANSI styling for plugin list/action
  feedback (`cb72d52`, `3ab1b42`)
- **NIU_ENV / BASH_ENV opt-in env files** for one-shot mode (#82, `ee1c410`)
- **Interactive shell easter eggs** (`30efd9a`)
- **Demo bundle** for plugin development (`f7d2943`)

### Fixes

- **Startup overhead**: skip framework hook dispatch when the runner is
  undefined (#80, `6fbee9f`)
- **CTLESC `\x11` leak** (upstream rubash v1.1.0): quoted assignment fast path
  leaked `\x11` into stored values, breaking `NIU_BINDKEYS="Ctrl+X:..."` parsing
  (`a313729b` in rubash)
- **Hook runner resilience**: hook runners survive user `set -eu` (`6ba7af0`)
- **REPL heredoc-body scanning**: fix completeness check for heredoc bodies
  spanning command-substitution boundaries (`6ba7af0`)
- **WinuxCmd auto-activation**: portable first-run activation with absolute-path
  activate script (`ca574ed`)
- **Panic hook**: best-effort console restore under `panic=abort` (`270d16b`)
- **Rename brand gate**: restore working version drift gate (`92e6724`)
- **$BASH path**: set `$BASH` to the running executable path (`0ea5513`)
- **Completion engine**: wire rubash completion engine, drop hardcoded builtin
  list (`316a8f6`)
- **Heredoc diagnostics** (upstream rubash v1.1.0): warning line numbers now
  use computed `warning_line` matching GNU `make_cmd.c:627` (`a313729b` in
  rubash; remaining gaps tracked in rubash issue #72)
- **Chinese path crash** (#84): `host_path_to_shell_path_with_root` panicked
  with `byte index is not a char boundary` when the shell root byte length
  fell inside a multi-byte character in the current directory path. Added
  `is_char_boundary` guard before slicing. Also fixed
  `longest_common_prefix` in the completion module which had the same class
  of bug when decrementing `prefix_len` through multi-byte characters.

### Build

- **Release binary size**: enable thin LTO + `panic=abort` (-35% size,
  `f94c48d`)
- **Warning suppression**: crate-level `#![allow]` for 11 legacy rubash warnings
  to unblock downstream CI (`a313729b` in rubash)

### Documentation

- Locale default decision for `${#var}` UTF-8 counting (`2f86b3e`)
- Trim redundant README sections; drop stale arm64/version claims (`ceb431e`)
- Add Built-ins & Fast Paths page (`1e35971`)
- Translate architecture page to English (`5a02b7b`)

### Dependencies

- Rubash upgraded from v1.0.0 to **v1.1.0**

## [1.0.1] - 2026-09-04

Initial stable release.
