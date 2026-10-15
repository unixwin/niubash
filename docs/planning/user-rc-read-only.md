# User RC Ownership and Read-Only Startup

Status: proposed contract and acceptance plan; this document does not implement
the behavior below. Existing startup, setup, and plugin-management behavior must
not be described as compliant until the corresponding tests pass.

## Ownership Boundary

`~/.niubashrc` is user-owned shell code, not a generated database. "Read-only"
describes Niubash's management of that file; it does not prevent users from
editing it or running their own startup commands. User aliases, functions,
exports, plugin selections, and prompt overrides remain valid shell code.

Niubash must not append, normalize, deduplicate, reformat, replace, truncate,
rename, or repair an existing user rc. A backup does not authorize replacement.
The same protection applies to legacy `~/.winuxshrc` and `~/.winshrc`, symlink
targets, and rc files explicitly selected by users.

Decision update: retire both `~/.winuxshrc` and `~/.winshrc` as startup inputs.
Do not read, migrate, copy, rename, repair, or delete them automatically. Preserve
their bytes as user files; do not recommend legacy fallback compatibility.
This proposal changes the earlier recommendation, not production behavior yet.

`~/.niubashrc` remains primary. If absent, the default is native defaults without
a user rc. A future explicit setup choice may select a specific `~/.bashrc`,
subject to the review below. This round implements no automatic fallback. File
existence is not consent; invalid or unreadable primary rc must not select a
different file. Startup must not create missing files or rewrite parse errors.

## Optional `.bashrc`: Explicit Choice, Not Safety Assurance

Future explicit setup may offer native defaults (the default), create-only
`.niubashrc`, or review and select an existing `.bashrc` when `.niubashrc` is
absent. Show the resolved path, warn about arbitrary code execution, and require
explicit confirmation. Non-interactive setup requires unambiguous opt-in;
existence or a syntax check is not approval. Never source a candidate to preview
it or discover settings. This round designs this choice but does not implement it.

Persist exactly one enum setting in structured `~/.niubash/config.toml`:
proposed `startup.rc-policy = "primary-only"` (default) or
`"bashrc-fallback"` (explicit setup opt-in). The key is proposed, not implemented.
Do not add competing booleans, marker files, or environment-based implicit grants.
Missing config/key, unknown enum values, unreadable config, wrong types, duplicate
keys, or malformed TOML fail closed to primary-only, never to fallback. Startup
does not repair or rewrite that state. Only explicit setup may persist or revoke
this policy; other config commands must preserve it without changing its value.

Setup must not modify either rc or source `.bashrc` as a trial. Selecting fallback
must not create an empty `.niubashrc`, which would suppress the selected fallback.
Creating a new primary rc is a different explicit create-only action. Display the
resolved candidate path and risks; cancellation or failed persistence leaves the
previous policy and both rc files unchanged. Provide no-rc recovery and revocation
without executing the selected file. Define path/symlink changes and consent
renewal before release.

The current `config.rs` writer around line 504 rebuilds a typed subset and loses
unknown keys and comments. It cannot be reused unchanged. Future explicit setup
must surgically update the one enum while preserving unrelated keys, comments,
and valid existing values, then publish atomically. Corrupt/unreadable config
must not be replaced with defaults; report inability to save and preserve bytes.
Other writers of that shared file must also preserve the enum and unrelated
content. This is required production follow-up, not a fix delivered here.

### Deterministic Selection Boundary

Apply this policy only where the existing startup contract would select the
default interactive user rc. Preserve existing `--norc`, `--rcfile`, POSIX,
login, and non-interactive boundaries, including their precedence and diagnostics.
Do not turn this into extra startup-file discovery or change NIU_ENV/BASH_ENV/ENV.
Do not additionally scan profiles, `/etc`, MSYS installations, or WSL homes.

Within that default-rc branch, primary-only never considers `.bashrc`. Under
explicit bashrc-fallback, inspect the primary directory entry without following
away a broken symlink: only genuine primary NotFound makes `.bashrc` eligible.
An existing `.niubashrc` exclusively owns the slot even when empty, unreadable,
syntactically invalid, a directory, or a dangling/cyclic/otherwise broken symlink.
Permission, traversal, IO, or ambiguous errors are not absence. Never interpret
an open/source failure after an existence decision as permission to try `.bashrc`;
resolve races conservatively without broadening selection. Missing `.bashrc`
leaves defaults; its own read/parse failure does not select another file. Preserve
existing diagnostics without promising recovery from arbitrary user code.

Audit scenarios, not claims about a particular user file:

- `BASH_VERSION`, `BASH_VERSINFO`, `$BASH`, `$0`, and `$-` guards can select code
  expecting GNU Bash features or builtins. Audit the actual Niubash identity and
  capability contract; a version string does not prove compatibility. Do not
  spoof versions to bypass guards.
- MSYS/Git Bash assumptions involving `MSYSTEM`, `OSTYPE`, `uname`, `/etc/profile`,
  `cygpath`, `winpty`, POSIX paths, or PATH rewrites may select the wrong host
  behavior. Inherited variables are not proof of an MSYS runtime. Review command
  resolution; do not fabricate MSYS state or auto-source its system profiles.
- Host installation commands such as `pacman`, `apt`, `brew`, `winget`, `wpm`,
  installer scripts, download pipelines, and tool-manager initialization may
  download, elevate, or rewrite host files. Inspect nested source/eval/command
  substitutions too. Never run candidates merely to scan them.
- `exec bash` or another shell can replace the session; `exit` terminates it.
  Redirections, traps, and options may persist in the current shell. Warn about
  replacement/termination and provide no-rc recovery; do not intercept or redefine
  these commands to manufacture a safety guarantee.
- Nested files, eval output, prompt hooks, completion loaders, and background
  jobs extend beyond the reviewed file. Static warnings are not a safety
  certificate and execution-based discovery must not be the default.

Arbitrary `.bashrc` cannot be declared safe to source: sourcing executes code in
the current shell, not a sandbox. A subprocess test cannot prove in-process
compatibility or undo external effects and may itself run installers. Any future
execution probe needs separate consent and bounded isolated fixtures. Offer
manual extraction of small reviewed shared configuration as an alternative.
Candidate source syntax checks are not a safety guarantee: even valid syntax
can execute arbitrary commands. `exit`, `exec`, environment changes, and external
side effects cannot be promised to roll back. Do not label a syntax pass or a
subprocess trial as safe-to-source certification.

### Selection and Persistence Acceptance Matrix

These are future isolated-fixture tests, not results from this documentation task.

| Case | Required result |
| --- | --- |
| No policy or primary-only; primary missing; `.bashrc` exists | Defaults only; do not open/execute `.bashrc`. |
| Explicit bashrc-fallback; primary genuinely NotFound | Only then consider the selected home `.bashrc`; no other search. |
| Either policy; primary exists and is empty | Primary exclusively selected; no fallback execution. |
| Either policy; primary unreadable, syntax-invalid, directory, broken symlink, IO/traversal error | No fallback; preserve applicable diagnostics and file bytes. |
| Primary disappears or changes during selection | No fallback on later open/source failure; no unsafe retry. |
| Fallback eligible but `.bashrc` absent, unreadable, invalid, or broken | Defaults or existing source error as appropriate; never scan another file. |
| `--norc`, explicit `--rcfile`, POSIX, login, script, `-c`, environment-file paths | Existing boundaries unchanged; opt-in must not introduce additional rc loads. |
| Both deprecated rc files, profiles, `/etc`, MSYS/WSL home candidates present | No new reads, migration, or execution of these candidates. |
| Unknown/missing enum, wrong type, duplicate key, corrupt/unreadable config | Fail closed to primary-only without config repair or startup writes. |
| Explicit setup select/revoke/cancel/fail | Only confirmed successful setup changes the enum; neither rc is modified or trial-executed; fallback selection creates no primary rc. |
| Config has unrelated keys/comments; any config writer runs | Preserve unrelated content and policy; only explicit setup changes policy. |
| Corrupt config or atomic-publication failure | Preserve old bytes and policy; report save failure, not success. |
| Synthetic version/MSYS/installer/exec/exit/hook fixtures | Warnings and no-preview behavior tested without executing real host rc or claiming safety. |

Observe selected-file markers, reads, child processes/network, rc bytes/metadata,
and configuration writes. Include symlink/permission and create-only race cases
on supported hosts. Non-mutating syntax checks must never run sourced code;
execution-only fixtures require separately bounded isolated test processes.

## Startup Contract

Interactive startup reads configuration and loads already-installed, explicitly
selected plugins. It must not perform installation, synchronization, adoption,
migration, restore, repair, registry normalization, or configuration generation.
It must not fetch sources, run package installers, or ask authorization questions.
Missing configuration or plugin state must leave a usable shell with native
fallbacks; it must not launch setup automatically or retry repair every session.

This restriction includes filesystem side effects reached through first-run
setup, plugin bootstrap, source-registry readers, and loader helpers. A function
named "read" must not silently snapshot corrupt state, create directories, or
write replacement state on the startup path. Diagnostics may identify an explicit
management command but must not invoke it. This is a configuration ownership
contract, not a blanket ban on independent history or explicitly enabled session
logging; such facilities must not mutate configuration as a side effect.

User-written rc code may intentionally invoke management commands. Niubash must
not inject such calls or require them for ordinary startup. Clearly distinguish
that user choice from product-owned startup behavior.

### Legacy `plugin sync --bootstrap`

Keep the old command spelling as a compatibility no-op so existing rc files do
not need to be edited by the product. It returns success silently and performs
no network access, registry parsing/repair, directory creation, spec adoption,
loader generation, or plugin installation. It must return before reaching the
normal sync path. It must not redirect to `plugin sync` or display a warning on
every shell launch. Mixed management flags with `--bootstrap` must never turn
the legacy startup invocation into a mutating command; document and test the
argument policy explicitly.

New templates must omit the bootstrap line. Removing this line from an existing
rc is a user action, not an automatic cleanup. The no-op is a safety shim, not
evidence that legacy plugin activation has been fully migrated.

## Plugin State and Generated Files

Keep the manifest-backed registry as the plugin control plane. Separate desired
selection, installed/pinned source state, authorization, and effective activation;
do not make the user rc a second machine-maintained source of truth.

All generated per-user plugin files belong under `~/.niubash/`, for example:

- `plugins.toml`: declarative source/asset/theme selections.
- `sources/registry.toml`: installed origins, versions, pins, and policy state.
- `sources/<id>/`: installed source trees and adapter-owned activation assets.
- `generated/plugins.niu`: a proposed generated activation entry point, not an
  existing stable interface. Its final name and loading API require review.

Only explicit management commands may materialize or replace generated files.
Generation should validate first and publish atomically, leaving the previous
valid generation intact on failure. Read failures, missing loaders, or corrupt
registry state must not cause startup writes. Preserve evidence and offer repair
through an explicit command rather than silently resetting pins or permissions.

Long-term loading should use a host-owned, documented read-only entry point;
users must be able to opt out and override its results. An optional manual
`source` line is also possible, but the product may only suggest it, never insert
it into an existing rc. Define ordering so user prompt and shell overrides win.
Neither route may execute both the old managed rc block and a new loader twice.

## Authorization and Integrity: Open Design Choice

Do not assume users must install, enable, and then run a separate `trust` command.
Shell source plugins execute with the user's privileges; authorization is not
sandboxing, and checksum pinning is not proof that code is safe.

| Model | Consent boundary | Benefit | Risk / required clarification |
| --- | --- | --- | --- |
| Explicit install authorizes | A clearly documented interactive or CLI install approves the named source and its stated activation behavior. | Smallest command sequence. | Fetch-only workflows must remain possible; declarative entries, imported collections, startup, and dependency discovery must not silently count as consent. |
| Explicit enable authorizes | Install fetches only; enable approves execution of the named source/assets. | Clear separation of acquisition and execution without an extra trust step. | Whole-source loaders may execute more than one selected file; show source identity and execution scope. |
| Advanced integrity policy | Optional pinned checksums or local signatures restrict which installed tree may execute. | Supports users who require reviewed revisions and drift rejection. | Update approval, key ownership, drift handling, and policy migration must be specified; a local signature is not publisher authentication. |

Recommended direction: evaluate explicit enable as the default authorization
boundary, with an explicit install-and-enable convenience path. Keep fetch-only
installation available. Compare install-as-authorization before finalizing the
CLI; do not introduce a mandatory extra trust step merely to preserve an old
boolean. Existing `trust` commands may remain as policy/compatibility interfaces,
but their migration semantics require an explicit decision.

Authorization and integrity must stay separate. Record what source/revision and
execution scope was approved; define whether a user-requested update approves a
new revision or requires renewed approval under the selected policy. Never turn
untrusted legacy records into authorized records implicitly. Missing or drifted
trees must not auto-repair during startup; an integrity-enforcing policy must
reject activation without making the whole shell unusable. Disabling must remain
possible without first approving execution.

## This Round: Minimum Containment

These are proposed implementation tasks, not changes delivered by this document:

1. Make legacy `sync --bootstrap` an early, silent, successful no-op, including
   when registry/spec state is absent, corrupt, or unwritable.
2. Remove product-owned startup installation/sync/migration/repair and automatic
   first-run wizard execution. Stop generating bootstrap calls in new templates.
3. Stop startup rc writes and remove both legacy lookup and migration. Preserve
   the old files untouched and leave missing primary files missing. Do not add
   automatic `.bashrc` fallback in this round.
4. Keep explicit plugin commands available, but prevent their managed-block
   writers from touching existing user rc files. Where generated-loader routing
   is not yet implemented, report the limitation or print manual instructions;
   do not claim success, silently lose selections, or fall back to editing rc.
5. Add bounded isolated-home tests for these boundaries. Do not claim the full
   configuration redesign or trust-model migration is finished by this stopgap.

## Long-Term Remaining Work and Acceptance

- Explicit `setup`, including reruns, presets, non-interactive defaults, and
  error recovery, must never overwrite or edit an existing rc. Even an overwrite
  flag is outside this contract. It may offer a separately named sample and a
  diff for the user to apply. Creation of a missing rc requires an explicit setup
  action and create-only semantics that cannot clobber a concurrently created rc.
- Define one plugin selection/control model and generated-loader ABI, including
  framework arrays, bash-it `enabled/`, whole-source loaders, direct files,
  theme precedence, disable, rollback, and failure atomicity. Resolve legacy rc
  blocks through an explicit user-reviewed transition, not startup rewriting.
- Finalize authorization defaults and optional integrity policies, update and
  collection/import consent, registry schema compatibility, and trust-command
  deprecation before changing historical grants or enforcement semantics.
- Startup tests must compare rc/legacy-file bytes and metadata and configuration
  directory contents before/after fresh and repeated launches. Cover absent rc,
  read-only rc/home, CRLF, custom code, malformed rc, symlink targets, missing or
  corrupt registry, offline operation, missing plugins, and integrity drift.
- Instrument child execution/network and configuration writes to prove startup
  does not install, fetch, sync, migrate, or repair. Verify quiet legacy bootstrap
  success and no recurring setup/authorization prompts. Assert no duplicate plugin
  execution and that user overrides remain effective.
- Management tests must show setup and plugin add/enable/disable/sync/update/
  restore cannot alter existing rc bytes, including failure paths. Test explicit
  setup create-only races and candidate-file generation separately.

## Implementation Review Map

- `crates/niubash-runtime/src/repl.rs`: first-run wizard and startup sequencing.
- `crates/niubash-runtime/src/shell.rs`: remove legacy lookup and migration;
  design future explicit `.bashrc` selection without automatic fallback.
- `crates/niubash-runtime/src/setup_wizard.rs`: setup output, rc writes, generated
  bootstrap, post-install authorization, and theme application.
- `crates/niubash-runtime/src/config.rs:504`: replace destructive typed-subset
  serialization before persisting the proposed rc-policy enum; audit every
  shared-config writer for unknown-key/comment and policy preservation.
- `src/main.rs`: legacy bootstrap dispatch and explicit plugin command behavior.
- `crates/niubash-runtime/src/plugins/{spec,sync,sources,assets,trust}.rs`:
  declarations, reconciliation, registry IO, activation writers, and integrity.
- `crates/niubash-runtime/src/doctor.rs`: read-only diagnostics versus explicit
  repair; inspect any diagnostic helper for hidden writes.
- `tests/{startup_files,plugin_spec_sync,plugin_assets,plugin_sources,smoke_1_3_0,defaults_floor}.rs`
  and wizard unit tests: contracts to revise and extend, not proof of compliance.
- `docs/planning/{plugin-system-direction,plugin-system-roadmap,setup-wizard-redesign,wizard-redesign}.md`
  and plugin guides: reconcile older managed-rc, automatic migration, and backup-
  then-overwrite advice when this proposal is adopted. No other document is
  silently superseded by the presence of this proposal alone.
