---
tags: [winuxsh, zsh, compatibility, roadmap, plugins]
created: 2026-07-17
status: active
---

# Zsh Compatibility Plan

> Goal: make winuxsh feel familiar to zsh users on native Windows, while keeping
> execution semantics in rubash and avoiding MSYS2/Git Bash/WSL isolation.

## Compatibility Policy

Winuxsh should aim for zsh compatibility in this order:

1. **Interactive UX compatibility**: keybindings, completions, prompt behavior,
   autosuggestions, syntax highlighting, history search.
2. **Profile compatibility**: import common `.zshrc` settings safely.
3. **Plugin compatibility**: support Oh My Zsh-style plugin discovery and
   translate/import useful plugin assets.
4. **Semantic compatibility**: do not implement zsh shell syntax in winuxsh;
   shell semantics remain rubash/bash-like.

This means `.zshrc` compatibility should be opt-in and diagnostic. Winuxsh
should not blindly source arbitrary zsh scripts as if it were zsh.

## Direct Compatibility Boundary

The goal is for a zsh user to point winuxsh at an existing zsh setup and get a
useful native Windows shell without rewriting everything by hand. The supported
model should be:

- **Read existing zsh files**: discover `.zshenv`, `.zprofile`, `.zshrc`,
  Oh My Zsh, `$ZSH_CUSTOM`, themes, and plugin directories.
- **Translate safe settings**: aliases, exports, PATH/path/fpath entries,
  plugin names, theme names, simple prompt escapes, completion assets, and
  editor mode.
- **Implement editor UX natively**: autosuggestions, syntax highlighting,
  history search, prompt indicators, completion menus.
- **Report unsupported constructs**: arbitrary ZLE widgets, `zmodload`, `zpty`,
  dynamic completion scripts, and plugins that require a real zsh interpreter.

This gives practical zsh compatibility while preserving the product contract:
Windows-native process behavior, bash-like execution through rubash, and no
MSYS2/Git Bash/WSL isolation.

## Proposed Config Surface

Decision: `.zshrc` should become the familiar user-facing compatibility entry
point, while `~/.winshrc.toml` remains the native winuxsh control plane. TOML is
not redundant: it is the deterministic place for safe import/apply state,
Windows-native overrides, agent-readable diagnostics, and rollback-safe managed
blocks. Winuxsh can read and translate `.zshrc`, but it should not execute zsh
startup files as the runtime authority.

Keep `~/.winshrc.toml` as the native authoritative config, then add a zsh
compatibility section:

```toml
[zsh]
enabled = true
zdotdir = "~"
import_zshrc = true
import_oh_my_zsh = true
plugins = ["git", "zsh-autosuggestions", "zsh-syntax-highlighting"]
compat_level = "safe" # safe | warn | experimental
```

Compatibility modes:

- `safe`: scan and translate known-safe records only; never execute sourced
  zsh files or plugin scripts.
- `warn`: import safe records and emit diagnostics for unsupported lines.
- `experimental`: allow additional translators for simple functions/prompts
  after tests exist; still do not source arbitrary zsh scripts at startup.

Also support familiar zsh environment variables where practical:

- `ZDOTDIR`
- `ZSH`
- `ZSH_CUSTOM`
- `ZSH_THEME`
- `CASE_SENSITIVE`
- `HYPHEN_INSENSITIVE`
- `ZSH_AUTOSUGGEST_*`
- `ZSH_HIGHLIGHT_STYLES`

## Phase 1 - Zsh Profile Scanner

Implementation interface audit: `DOCS/zsh-compatibility-interface-audit.md`.

Current implementation status: scanner and `--zsh-compat-report` /
`--zsh-compat-report-json` CLI are implemented on `codex/zsh-compat-scanner`.
Opt-in startup import is available behind `[zsh].enabled = true` plus
`[zsh].auto_apply = true` for known-safe env/PATH records and aliases only.
Completion assets, theme hints, and editor hints remain report-only until their
native translators are implemented.

Build a scanner/parser for zsh profile files. It should read but not execute:

- `${ZDOTDIR:-$HOME}/.zshrc`
- optionally `.zshenv` only for safe env assignments
- Oh My Zsh template patterns

Supported first:

- `export KEY=value`
- `alias name='value'`
- simple `PATH=...` / `path=(...)`
- `fpath=(...)`
- `plugins=(...)`
- `ZSH_THEME="..."`
- `source $ZSH/oh-my-zsh.sh` as a signal, not a direct source operation
- `bindkey -e` / `bindkey -v`
- simple `zstyle '<context>' <key> <value...>`

Output:

- an import report
- native config suggestions
- imported aliases/completion dirs/theme settings where safe
- a stable diagnostic format suitable for agents and CI snapshots
- opt-in safe application path:
  - env/PATH records apply before `rubash::Executor::new()`
  - `winuxcmd` PATH injection still runs after zsh PATH import so coreutils win
  - imported aliases apply after executor construction through rubash `alias`
    builtin semantics
  - native `.winshrc.toml` aliases apply last and override imported names

Tests:

- fixture `.zshrc` files for Oh My Zsh template, simple aliases, plugin arrays,
  fpath, zstyle, bindkey.
- safe apply tests for PATH de-duplication, env whitelist filtering, and rubash
  alias installation.

## Phase 2 - Oh My Zsh Layout Importer

Implementation status: Phase 2a is implemented on `codex/zsh-compat-scanner`.
Static completion assets already discovered by the scanner are translated into
winuxsh `CommandDef` records. This covers `#compdef`, simple `compdef`, and
simple `_arguments` option specs. It does not execute zsh completion functions
and does not attempt dynamic `compadd`, `_describe`, cache, or ZLE behavior.
Translated definitions merge with built-in winuxcmd defaults, while user TOML
completion definitions still take highest priority.

Support Oh My Zsh-like discovery:

- `$ZSH/plugins/<plugin>/<plugin>.plugin.zsh`
- `$ZSH/plugins/<plugin>/_<plugin>`
- `$ZSH_CUSTOM/plugins/<plugin>/...`
- `$ZSH/themes/<theme>.zsh-theme`
- `$ZSH_CUSTOM/themes/<theme>.zsh-theme`

Import behavior:

- `_cmd` completion files become candidate zsh completion assets.
- simple alias-only plugin snippets become aliases.
- `compdef` mappings become command completion metadata.
- unsupported ZLE/zmodload/zpty lines are reported and skipped.

Do not execute arbitrary plugin scripts during startup.

Plugin compatibility should be tiered:

| Tier | Plugin shape | Initial behavior |
| --- | --- | --- |
| 1 | completion-only `_cmd` files and `#compdef` | import/translate |
| 1 | alias-only plugins | import aliases |
| 2 | simple prompt/theme snippets | translate common prompt escapes |
| 2 | simple shell functions compatible with rubash/bash | optional later translator |
| 3 | zsh-autosuggestions / zsh-syntax-highlighting | native reedline/rubash implementation |
| 4 | ZLE widgets, `zmodload`, `zpty`, deep zsh internals | report and skip |

Implementation status: Phase 2b is implemented on `codex/zsh-compat-scanner`.
The import report now includes explicit plugin tier metadata:
`completion_only`, `alias_only`, `alias_and_completion`, `native_ux`,
`partial`, `unsupported`, and `missing`. This keeps startup safe while giving
users and agents a concrete compatibility map for common Oh My Zsh plugins.

## Phase 3 - Zsh Completion Compatibility

Add an importer for zsh completion files:

- parse `#compdef` headers
- parse common `_arguments` forms where practical
- parse simple `compdef _foo foo` mappings
- preserve user override order
- emit/derive winuxsh TOML completion definitions

Fallback:

- if a zsh completion is too dynamic, keep it as unsupported and point users to
  winuxcmd/help-derived or TOML completion definitions.

## Phase 4 - Native Autosuggestions

Implementation status: Phase 4a is implemented on `codex/zsh-compat-scanner`.
The first implementation is intentionally native and narrow: winuxsh now wires
a reedline history hinter into the REPL, honors familiar zsh-autosuggestions
configuration names where they map cleanly, and keeps completion/match-prev-cmd
strategies report-only until tests exist.

Implement zsh-autosuggestions behavior natively in reedline:

- history strategy first
- optional completion strategy later
- muted right-of-cursor suggestion
- accept full suggestion with common forward/end widgets
- partial accept with word-forward actions if reedline exposes enough hooks

Honor familiar config names when set:

- `ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE`
- `ZSH_AUTOSUGGEST_STRATEGY`
- `ZSH_AUTOSUGGEST_BUFFER_MAX_SIZE`
- selected widget lists if they can map to reedline events

Current supported subset:

- `history` strategy via native reedline hints.
- `ZSH_AUTOSUGGEST_HIGHLIGHT_STYLE` subset: `fg=`, `bg=`, `bold`,
  `underline`, `italic`, `standout` / `reverse`.
- `ZSH_AUTOSUGGEST_BUFFER_MAX_SIZE`.
- Native TOML override under `[zsh.autosuggestions]`.

## Phase 5 - Native Syntax Highlighting

Implementation status: Phase 5a is implemented on `codex/zsh-compat-scanner`.
Winuxsh now provides a native reedline highlighter for the
zsh-syntax-highlighting `main` highlighter subset. It does not source
`zsh-syntax-highlighting.zsh` and does not depend on ZLE variables such as
`BUFFER` or `region_highlight`.

Implement zsh-syntax-highlighting-like behavior natively:

- command position known/unknown
- paths and path prefixes
- strings and quotes
- variables and command substitutions
- redirections and pipes
- comments
- errors/incomplete syntax where rubash tokenizer/parser can expose them

Honor a subset of:

- `ZSH_HIGHLIGHT_STYLES`
- `ZSH_HIGHLIGHT_HIGHLIGHTERS`

Current supported subset:

- command position known/unknown highlighting
- shell builtins, reserved words, command separators, redirections
- existing paths and path prefixes
- single/double quoted arguments, unquoted variables, command substitutions
- single/double hyphen options, assignments, comments
- `ZSH_HIGHLIGHT_STYLES[key]=...` scan/import for supported style keys
- native TOML override under `[zsh.syntax_highlighting]`

Do not run `zsh-syntax-highlighting.zsh`; it depends on ZLE internals and zsh
parameters like `BUFFER`, `PREBUFFER`, and `region_highlight`.

Later phases can add zsh's non-default highlighters such as `brackets`,
`pattern`, `regexp`, `cursor`, and `line` once the main highlighter is stable.

## Phase 6 - Prompt and Theme Compatibility

Implementation status: Phase 6a is implemented on `codex/zsh-compat-scanner`.
Winuxsh now translates common static zsh prompt assignments into native
winuxsh prompt templates and reports unsupported dynamic segments. It does not
source theme scripts or execute prompt substitutions.

Translate common zsh prompt/theme forms:

- `PROMPT`
- `RPROMPT`
- `%~`, `%n`, `%m`, `%#`
- `%F{color}`, `%f`, `%B`, `%b`
- Oh My Zsh `ZSH_THEME`
- common Git prompt variables

Native output should become winuxsh prompt/theme config, not arbitrary zsh theme
execution.

First-pass scope:

- scan `.zshrc` and simple Oh My Zsh theme files for `PROMPT` / `PS1` and
  `RPROMPT` / `RPS1`.
- translate common prompt escapes into winuxsh placeholders:
  `{user}`, `{host}`, `{cwd}`, `{symbol}`.
- strip or report color/style escapes and unsupported dynamic command
  substitutions.
- let native `[shell].prompt_format` override imported zsh prompts.
- expose translated prompts in `--zsh-compat-report` and
  `--zsh-compat-report-json`.

Current supported subset:

- `PROMPT` / `PS1` and `RPROMPT` / `RPS1` from `.zshrc`.
- static Oh My Zsh theme files under `$ZSH/themes` or `$ZSH_CUSTOM/themes`.
- prompt escapes `%n`, `%m`, `%M`, `%~`, `%/`, `%d`, `%c`, `%C`, `%1~`,
  `%2~`, `%3~`, `%#`, and `%%`.
- color/style escapes `%F{...}`, `%K{...}`, `%f`, `%k`, `%B`, `%b`, `%U`,
  `%u`, `%S`, `%s`, `%{...%}` are stripped safely.
- unsupported prompt substitutions such as `${...}`, backticks, `%D{...}`,
  `git_prompt_status`, `git_prompt_ahead`, and conditional `%(... )` are
  reported as unsupported prompt segments.
- native `[shell].prompt_format` and `[shell].right_prompt_format` remain
  authoritative over imported zsh prompts.

Implementation status: Phase 6b is implemented on
`codex/zsh-compat-scanner`.

Phase 6b adds a native bridge for common Oh My Zsh Git prompt forms:

- translate `$(git_prompt_info)` and escaped `\$(git_prompt_info)` to a native
  `{git_prompt}` placeholder.
- scan `ZSH_THEME_GIT_PROMPT_PREFIX` and `ZSH_THEME_GIT_PROMPT_SUFFIX` from
  `.zshrc` or static theme files, stripping zsh color/style escapes.
- render `{git_prompt}` from native `.git/HEAD` discovery instead of executing
  zsh Git helper functions.
- keep detailed `git_prompt_status`, `git_prompt_ahead`, async Git, dirty, and
  per-file status segments report-only until a tested native status provider
  exists.

## Phase 7 - Oh-My-Winuxsh Compatibility Layer

Implementation status: Phase 7a through Phase 7e are implemented on
`codex/zsh-compat-scanner`.

Phase 7a adds a safe local import-plan command:

- `winuxsh --zsh-compat-import-plan` scans the current zsh setup and prints a
  reviewable `.winshrc.toml` patch.
- The command must not write `~/.winshrc.toml` or copy plugin/theme files.
- The generated patch should enable `[zsh]` safe auto-apply, preserve zsh
  plugin names, and include native prompt/editor/alias translations where they
  are already supported.
- Unsupported features remain visible through `--zsh-compat-report`.

Phase 7b adds an explicit local apply command:

- `winuxsh --zsh-compat-import-apply` writes the same generated import block to
  `~/.winshrc.toml`.
- The command must create a timestamped backup before writing.
- The command may replace only the previous winuxsh-managed zsh import block;
  user-authored TOML outside that block must remain untouched.
- If the generated block would duplicate existing user-authored TOML tables,
  the command must fail before writing and tell users to merge the plan
  manually.
- The command must stay explicit and one-shot. Startup must continue to read
  native TOML only and must not mutate user config.

Phase 7c adds a read-only status command:

- `winuxsh --zsh-compat-import-status` inspects `~/.winshrc.toml` without
  writing it.
- The command reports whether the config exists, whether the winuxsh-managed
  import block is missing/present/malformed, whether current TOML parses, and
  whether a new apply would add, replace, or fail before writing.
- The command reports discovered backup files so users and agents can see
  whether a previous apply created a rollback point.

Phase 7d adds a read-only rollback plan command:

- `winuxsh --zsh-compat-import-rollback-plan` inspects the backup files created
  by `--zsh-compat-import-apply`.
- The command prints the latest rollback source and destination, plus the
  exact PowerShell copy command a user or agent can run.
- The command must not restore files automatically. A future explicit rollback
  apply command can be added after the plan command is tested.

Phase 7e adds a read-only doctor command:

- `winuxsh --zsh-compat-doctor` aggregates the zsh scan report, import status,
  and rollback plan into a compact operator-facing summary.
- The command should answer: what was discovered, whether `apply` is safe, what
  blocks it if not, and whether a rollback point exists.
- The command must remain read-only and must not replace the detailed report,
  JSON report, import-plan, status, or rollback-plan commands.

Once the importer works, build a local package layer:

- install/import zsh-compatible completion packs
- install/import themes
- install native autosuggestion/highlighting modules
- produce import reports for existing Oh My Zsh setups

This should start local-only. Online registry behavior comes later.

## Phase 8 - Native Plugin Packs

Implementation status: Phase 8a is implemented on
`codex/zsh-compat-scanner`.

Phase 8a starts the local-only native plugin pack layer with the Oh My Zsh
`git` plugin:

- If `.zshrc` declares `plugins=(git)` but no readable Oh My Zsh `git`
  plugin directory is available, winuxsh should still provide a conservative
  native alias pack.
- Native aliases must be marked with `origin = "native-plugin:git"` in the
  report and import plan.
- Native aliases must not override aliases already discovered from the user's
  zsh files.
- No zsh plugin scripts are executed and no Oh My Zsh source is vendored.

Later Phase 8 work can add native packs for `docker`, `npm`, `node`, `python`,
`pip`, `kubectl`, and related completion metadata after each pack has tests.

Implementation status: Phase 8b is implemented on
`codex/zsh-compat-scanner`.

Phase 8b adds the Oh My Zsh `docker` plugin as a conservative native alias pack:

- If `.zshrc` declares `plugins=(docker)` but no readable Oh My Zsh `docker`
  plugin directory is available, winuxsh provides static Docker aliases derived
  from the upstream Oh My Zsh plugin.
- Native aliases are marked with `origin = "native-plugin:docker"` in the report
  and import plan.
- Native Docker aliases do not override aliases already discovered from the
  user's zsh files.
- The dynamic Docker completion/cache logic in the Oh My Zsh plugin remains
  report-only future work; winuxsh does not execute that zsh code.

Implementation status: Phase 8c is implemented on
`codex/zsh-compat-scanner`.

Phase 8c starts the dynamic plugin bridge by scanning, not executing, known
dynamic completion generators:

- The scanner now records lines such as `kubectl completion zsh`,
  `docker completion zsh`, and `source <(tool completion zsh)` as structured
  dynamic completion sources.
- Plugins with alias assets plus dynamic completion generators are classified as
  partial and expose `dynamic_completions_required` in their capabilities.
- Dynamic completion functions that depend on zsh internals such as `compadd`,
  `_describe`, `_values`, `_wanted`, and `_comps[...]` remain unsupported until
  winuxsh has a native completion-provider API.
- The next implementation step is a native cache/provider that can run explicit
  external generators with timeout and translate their zsh output; startup must
  still not execute arbitrary plugin scripts.

Why static packs still matter:

- Many Oh My Zsh plugins are mostly aliases and static `_arguments` completion
  metadata, so users get immediate value without running zsh code.
- Static import creates a safe compatibility floor and preserves user overrides.
- Dynamic support should layer on top as explicit native providers for common
  CLIs, not as zsh script execution.

Implementation status: Phase 8d is implemented on
`codex/zsh-compat-scanner`.

Phase 8d adds the first dynamic completion translation seam:

- `dynamic_completion_defs_from_report_with_runner` accepts structured dynamic
  sources from the scanner and an injected runner.
- The runner output is treated as zsh completion text and translated through the
  same `_arguments` parser used for static completion assets.
- Tests cover a `kubectl completion zsh`-style generator without running the
  real `kubectl` binary.
- This deliberately stops short of startup execution; the next phase should add
  a native cache/provider with command allowlisting, timeout, stderr capture, and
  stale-cache fallback.

Implementation status: Phase 8e is implemented on
`codex/zsh-compat-scanner`.

Phase 8e adds the first safe dynamic completion runner:

- Dynamic generators still do not run by default.
- `dynamic_completion_defs_from_report_with_options` executes only explicitly
  allowed command names from structured dynamic completion sources.
- The runner captures stdout/stderr through temporary files, polls the child
  process, and kills it on timeout to avoid hanging the shell or filling pipes.
- Tests use a local fake `dyncli.cmd completion zsh` generator to prove the
  provider can execute, translate, and reject non-allowlisted commands.
- The next phase should persist generated zsh completion output in a cache and
  wire selected providers into startup behind config.

## Phase 9 - Configurable Dynamic Completion Provider

Implementation status: Phase 9a is implemented on
`codex/zsh-compat-scanner`.

Phase 9 turns the dynamic completion bridge into an opt-in native provider:

```toml
[zsh.dynamic_completions]
enabled = true
commands = ["docker", "kubectl"]
timeout_millis = 1500
cache_ttl_secs = 86400
```

Rules:

- Dynamic generators remain disabled by default.
- `commands` is an allowlist; scanner-discovered generators outside it are not
  executed.
- Generated zsh completion output is cached before translation, so startup can
  reuse recent output without re-running slow CLIs.
- Cache misses may run only the structured command/args discovered by the
  scanner, such as `docker completion zsh` or `kubectl completion zsh`.
- Timeout, stderr capture, and stale-cache fallback must protect interactive
  startup from hanging or noisy tools.
- User TOML completion definitions keep highest priority over static and dynamic
  zsh-derived definitions.

Current implementation:

- `[zsh.dynamic_completions]` parses `enabled`, `commands`, `timeout_millis`,
  `cache_ttl_secs`, and `cache_dir`.
- `Shell::new()` loads dynamic zsh-derived completion definitions only when zsh
  safe import has a scan report and dynamic completions are explicitly enabled.
- Generator output is cached under `~/.winuxsh/cache/zsh-completions` by default,
  with fresh-cache reuse and stale-cache fallback when the generator fails.
- Tests cover config parsing, allowlist rejection, successful `.cmd` generator
  execution, and cache reuse after the generator disappears.

## Phase 10 - Real-World Plugin Presets

Implementation status: Phase 10a is implemented on
`codex/zsh-compat-scanner`.

Phase 10 adds per-plugin presets for high-value Oh My Zsh plugins where a safe
native mapping is clear:

- `plugins=(kubectl)` without a readable Oh My Zsh plugin directory now receives
  a conservative native alias pack derived from the upstream Oh My Zsh kubectl
  plugin.
- Function-like aliases and `eval`/`compdef` helper functions remain skipped.
- The native kubectl preset also registers the structured dynamic completion
  source `kubectl completion zsh`.
- Import plans now emit a disabled `[zsh.dynamic_completions]` suggestion block
  with discovered command allowlist entries, so users can review and opt in.
- User aliases discovered from `.zshrc` still win over native preset aliases.

Implementation status: Phase 10b is implemented on
`codex/zsh-compat-scanner`.

Phase 10b adds the Oh My Zsh `npm` plugin as a conservative native preset:

- `plugins=(npm)` without a readable Oh My Zsh plugin directory receives safe
  npm aliases such as `npmg`, `npmS`, `npmD`, `npmR`, `npmrd`, and `npmrb`.
- `npmE` is intentionally skipped because it relies on command substitution and
  PATH mutation inside the alias body.
- The npm F2 install/uninstall toggle is marked as `native_ux_required`; it
  depends on ZLE `BUFFER`, `CURSOR`, `bindkey`, and history widgets, so it should
  become a future reedline-native shim rather than sourced zsh code.
- User aliases discovered from `.zshrc` still win over native preset aliases.

Implementation status: Phase 10c is implemented on
`codex/zsh-compat-scanner`.

Phase 10c separates static plugin import from multiple dynamic plugin shapes:

- Static import remains useful for aliases, simple `_arguments` completion
  assets, prompt hints, and safe config translation. This is the compatibility
  floor, not the whole plugin story.
- `script_generator` dynamic completions, such as `kubectl completion zsh`, can
  run through the existing allowlisted/cache-backed `[zsh.dynamic_completions]`
  path because they generate zsh completion script text that winuxsh can
  translate at startup.
- `runtime_provider` dynamic completions, such as the Oh My Zsh npm plugin's
  `npm completion -- "${words[@]}"`, depend on the current input buffer and
  must become native winuxsh/reedline completion providers. They are reported in
  import plans but are not enabled through `[zsh.dynamic_completions]`.
- ZLE widget plugins that read or write `BUFFER`, `CURSOR`, `zle -N`, or
  `bindkey` require native reedline widgets rather than zsh script execution.
- Lifecycle plugins that use `add-zsh-hook`, `precmd`, `preexec`, or `chpwd`
  require native winuxsh hook points before they can be meaningfully imported.
- Autoloaded zsh functions are marked separately so future plugin work can
  decide whether to translate a known pattern, replace it with a native preset,
  or leave it unsupported.

This gives the project a concrete plugin compatibility map:

| Shape | Example | Current behavior | Needed native surface |
| --- | --- | --- | --- |
| Static alias/config | `git`, `docker`, `npm` aliases | import/apply safely | existing TOML/import layer |
| Static `_arguments` completion | simple `_cmd` assets | translate to `CommandDef` | existing completion translator |
| Script generator completion | `kubectl completion zsh` | allowlisted run + cache + translate | existing dynamic provider |
| Runtime completion provider | `npm completion -- "${words[@]}"` | opt-in `[zsh.runtime_completions]` provider | completion runtime provider API |
| ZLE widget/keybinding | npm F2 toggle, history widgets | report/native UX required | reedline widget/keybinding shims |
| Lifecycle hooks | `precmd`, `preexec`, `chpwd` | report only | native shell lifecycle hooks |
| Autoload/function helpers | `_foo`, `prompt_info`, `alias-finder()` | report only | reviewed native helper/function translators |

## Phase 11 - Runtime Completion Providers

Implementation status: Phase 11a is implemented on
`codex/zsh-compat-scanner`.

Phase 11a connects the second dynamic plugin shape: providers that ask a CLI for
candidate words at Tab time, using the current command buffer.

```toml
[zsh.runtime_completions]
enabled = true
commands = ["npm"]
timeout_millis = 1000
```

Current behavior:

- Runtime providers remain disabled by default and require an explicit command
  allowlist in native TOML.
- The scanner/import plan reports npm-style providers as
  `[zsh.runtime_completions]`, separate from `[zsh.dynamic_completions]`.
- `Shell::new()` registers a native completion provider only after a safe zsh
  scan finds a matching `runtime_provider` source and the command is allowlisted.
- The provider appends current words to the discovered command shape, e.g.
  `npm completion -- npm run b`, then filters stdout lines by the current word.
- Runtime execution uses a timeout, stderr/stdout capture, and Windows PATH
  lookup for `.exe`, `.cmd`, and `.bat` so npm-style shims work natively.
- It does not source zsh code, evaluate `compadd`, or execute arbitrary plugin
  scripts.

Why this matters:

- Static alias/completion packs are still useful for many Oh My Zsh plugins, but
  they are not enough for CLIs whose candidates depend on project state,
  subcommands, package scripts, clusters, or remote context.
- Script generators such as `kubectl completion zsh` are startup/cache oriented;
  runtime providers such as `npm completion -- "${words[@]}"` are interactive
  and must be queried with the current line.
- ZLE widgets and lifecycle hooks remain a separate class; they need reedline
  widget shims and shell lifecycle hook points rather than completion providers.

## Phase 12 - Native Lifecycle Hooks

Implementation status: Phase 12a is implemented on
`codex/zsh-compat-scanner`.

Phase 12a adds native REPL lifecycle hook points that make zsh hook-shaped
plugins actionable without executing zsh plugin scripts:

```toml
[hooks]
startup = ["winuxfetch"]
precmd = ["echo before prompt"]
preexec = ["echo before command"]
chpwd = ["echo directory changed"]
```

Current behavior:

- `startup` hooks run once after REPL startup setup and before the first prompt.
- `precmd` hooks run before each interactive prompt render.
- `preexec` hooks run before each non-empty interactive command.
- `chpwd` hooks run after an interactive command changes the current directory.
  Directory-change detection uses rubash's shell `PWD`, not the host process
  cwd, so it follows shell-visible state even when the executor restores the
  process working directory after command execution.
- Hook scripts are native winuxsh/rubash scripts from `~/.winshrc.toml`; winuxsh
  does not source `precmd()`, `preexec()`, `chpwd()`, or `add-zsh-hook` bodies
  from zsh plugins.
- Hook context is exposed through temporary shell variables:
  `WINUXSH_REPL_STARTUP`, `WINUXSH_LAST_EXIT_CODE`,
  `WINUXSH_PREEXEC_COMMAND`, `WINUXSH_OLDPWD`, and `WINUXSH_PWD`.
- The hook path is REPL-only. `winuxsh -c ...` and script-file execution remain
  deterministic and do not run interactive lifecycle hooks.

Why this matters:

- Many zsh plugins are not only completions; they rely on lifecycle hooks to
  refresh prompt state, directory-local config, virtualenv status, or tool
  context.
- Winuxsh now has a native target surface for future safe translators and
  native presets, while preserving the rule that arbitrary zsh function bodies
  are not executed.
- The next lifecycle step is a compatibility translator for very small,
  auditable hook patterns, plus native presets for common hook-based plugins.

Implementation status: Phase 12b is implemented on
`codex/zsh-compat-scanner`.

Phase 12b makes hook-shaped dynamic plugins visible and actionable in the
compatibility report:

- scan `add-zsh-hook precmd/preexec/chpwd <function>` registrations.
- scan `precmd_functions`, `preexec_functions`, and `chpwd_functions` arrays.
- scan direct `precmd()`, `preexec()`, and `chpwd()` function definitions.
- classify hook-only plugins as native UX required instead of opaque
  unsupported plugins.
- emit `native_hooks` in the JSON/plain report and commented `[hooks]` TODOs in
  `--zsh-compat-import-plan`.

Winuxsh still does not copy or execute zsh hook function bodies. The generated
plan deliberately contains disabled TODO scripts so users or future native
presets can translate reviewed hook behavior into native winuxsh/rubash hook
commands.

## Phase 13 - Native ZLE Widget Suggestions

Implementation status: Phase 13a is implemented on
`codex/zsh-compat-scanner`.

Phase 13a makes ZLE widget and keybinding plugins visible as native reedline
migration targets instead of plain unsupported zsh internals.

Initial scope:

- scan `zle -N <widget> [function]` widget registrations.
- scan custom `bindkey <key> <widget>` and `bindkey -M <keymap> <key> <widget>`
  mappings.
- emit structured `native_widgets` records in report output.
- classify widget-only plugins as native UX required, not as generic
  unsupported plugins.
- emit disabled TODOs in `--zsh-compat-import-plan` for future native reedline
  widget/keybinding shims.

Out of scope for Phase 13a:

- executing ZLE functions.
- translating arbitrary widget function bodies.
- implementing a keybinding DSL in TOML before reedline-native shims are chosen.

## Phase 14 - Native ZLE Widget Bindings

Implementation status: Phase 14a is implemented on
`codex/zsh-compat-scanner`.

Phase 14a turns recognized ZLE widget suggestions into opt-in reedline
keybindings. This is the first phase where dynamic widget-shaped plugins become
user-visible behavior instead of report-only diagnostics.

Planned config:

```toml
[zsh.native_widgets]
enabled = true
presets = ["autosuggestions", "history_substring_search"]
import_bindkeys = true
```

Current mapping:

- `autosuggest-accept` -> reedline `HistoryHintComplete`.
- `autosuggest-execute` -> accept hint then enter.
- `autosuggest-partial-accept` -> reedline `HistoryHintWordComplete`.
- `history-substring-search-up` / `history-substring-search-down` -> native
  reedline history traversal as the closest safe first pass.

Rules:

- disabled by default; import plan may suggest the block but not enable it.
- only recognized widget names are mapped.
- only safe key sequences are parsed (`^X`, `^ `, arrow escape forms, and
  plain one-character keys).
- no arbitrary ZLE function bodies are executed.
- custom keybindings are imported only when both `[zsh].auto_apply = true` and
  `[zsh.native_widgets].enabled = true` are set.

Implementation status: Phase 14b is implemented on
`codex/zsh-compat-scanner`.

Phase 14b recognizes common native UX plugin declarations even when the
corresponding Oh My Zsh plugin directory is not available locally:

- `zsh-autosuggestions`
- `zsh-history-substring-search`
- `zsh-syntax-highlighting`
- `fast-syntax-highlighting`
- `fzf-tab`

These plugins are reported as `NativeUx` / `Tier3Native` instead of `Missing`.
For widget-backed plugins, the import plan suggests disabled
`[zsh.native_widgets]` presets so users can opt into the reedline-native
behavior without sourcing any zsh plugin code.

## Phase 15 - Autoloaded Function Suggestions

Implementation status: Phase 15a is implemented on
`codex/zsh-compat-scanner`.

Phase 15a makes autoload/function-shaped plugins visible as native migration
targets instead of opaque unsupported zsh scripts:

- scan `autoload -Uz ...` and `autoload -U +X ...` declarations.
- scan direct zsh function definitions such as `function name() { ... }` and
  `name() { ... }`.
- classify discovered functions as completion helpers, lifecycle helpers,
  widget helpers, prompt helpers, or generic helpers.
- emit structured report/JSON records and commented import-plan TODOs.

Rules:

- winuxsh still never sources zsh function bodies directly.
- function suggestions are an index for future native translators, presets, or
  runtime providers; they are not enabled behavior by themselves.
- `.zshrc` remains the familiar compatibility input, while TOML remains the
  safe native control plane for explicit apply/rollback and agent-readable
  diagnostics.

## Phase 16 - Native Dynamic Plugin Presets

Implementation status: Phase 16a is implemented on
`codex/zsh-compat-scanner`.

Planned direction: turn the safest dynamic plugin shapes from report-only into
explicit native presets. These presets should be opt-in, Windows-native, and
implemented through winuxsh/rubash/reedline surfaces rather than by sourcing zsh
plugin scripts.

Phase 16a adds a native `direnv` preset:

- recognize `plugins=(direnv)` as a native dynamic plugin candidate even when
  the Oh My Zsh plugin directory is not installed locally.
- suggest a disabled `[zsh.native_plugins]` import-plan block with
  `presets = ["direnv"]`.
- when explicitly enabled, run `direnv export bash` at native prompt/chpwd hook
  points and apply the generated bash-compatible environment script through
  rubash.
- keep missing `direnv` silent/best-effort so interactive startup is not noisy.

Rules:

- disabled by default and never enabled only because `.zshrc` mentions the
  plugin.
- no Oh My Zsh `direnv.plugin.zsh` sourcing.
- use `direnv export bash`, not `direnv export zsh`, because rubash owns shell
  semantics.

Implementation status: Phase 16b is implemented on
`codex/zsh-compat-scanner`.

Phase 16b targets the next dynamic shape: preexec helper plugins. Static
aliases and completion files are only the compatibility floor; many useful zsh
plugins are event-driven. They watch the command line before execution, refresh
state before each prompt, respond to directory changes, or bind ZLE widgets.
Winuxsh should map those shapes onto native hook/provider/widget surfaces rather
than trying to run the zsh implementation.

Phase 16b adds a native `alias-finder` preset:

- recognize `plugins=(alias-finder)` as a native dynamic plugin candidate even
  when the Oh My Zsh plugin directory is not installed locally.
- suggest a disabled `[zsh.native_plugins]` import-plan block with
  `presets = ["alias-finder"]`.
- when explicitly enabled, run at the native `preexec` lifecycle point and look
  for already-loaded aliases whose value exactly matches the typed command.
- print a quiet suggestion such as `winuxsh: alias available: gst='git status'`
  before command execution.
- keep zstyle options such as `longer`, `cheaper`, and `autoload` out of scope
  until the exact-match preset has tests and real usage feedback.

Rules:

- disabled by default and never enabled only because `.zshrc` mentions the
  plugin.
- no Oh My Zsh `alias-finder.plugin.zsh` sourcing.
- use winuxsh's native alias mirror from imported `.zshrc` aliases and native
  TOML aliases; rubash remains the authority for command execution.

Implementation status: Phase 16c is implemented on
`codex/zsh-compat-scanner`.

Phase 16c targets external state providers that ship zsh init snippets but can
be expressed safely as native winuxsh/rubash behavior.

Phase 16c adds a native `zoxide` preset:

- recognize `plugins=(zoxide)` as a native dynamic plugin candidate even when
  the Oh My Zsh plugin directory is not installed locally.
- suggest a disabled `[zsh.native_plugins]` import-plan block with
  `presets = ["zoxide"]`.
- when explicitly enabled, provide a native `z` command shim that runs
  `zoxide query`, converts native Windows drive paths back to rubash shell
  paths, and then `cd`s through rubash.
- track shell-visible directory changes through native prompt/chpwd hook points
  by running `zoxide add <PWD>` best-effort and silently when `zoxide` is not
  installed.
- translate simple `/c/...` shell paths to `C:/...` for native Windows
  `zoxide.exe add`, and translate `C:/...` query results back to `/c/...` for
  rubash `cd`.

Rules:

- disabled by default and never enabled only because `.zshrc` mentions the
  plugin.
- no Oh My Zsh `zoxide.plugin.zsh` sourcing and no `eval "$(zoxide init zsh)"`.
- use a winuxsh-native command shim and native lifecycle hooks instead of zsh
  function bodies.
- keep interactive selector behavior such as `zi` out of scope until there is a
  tested native UI/provider surface.

Implementation status: Phase 16d is implemented on
`codex/zsh-compat-thefuck`.

Phase 16d targets command-correction plugins that combine shell history with
ZLE buffer editing.

Phase 16d adds a native `thefuck` preset:

- recognize `plugins=(thefuck)` as a native dynamic plugin candidate even when
  the Oh My Zsh plugin directory is not installed locally.
- suggest a disabled `[zsh.native_plugins]` import-plan block with
  `presets = ["thefuck"]`.
- when explicitly enabled, provide a native `fuck` command shim that calls
  `thefuck` with either explicit arguments or the previous interactive command.
- execute the correction returned by `thefuck` through rubash so aliases,
  `cd`, PATH, and Windows-native command execution still follow winuxsh
  semantics.
- keep the Oh My Zsh `Esc Esc` ZLE buffer widget report-only until there is a
  tested reedline-native correction widget.

Rules:

- disabled by default and never enabled only because `.zshrc` mentions the
  plugin.
- no Oh My Zsh `thefuck.plugin.zsh` sourcing and no cached `thefuck --alias`
  source file execution.
- use native last-command state plus a command shim instead of zsh `fc`,
  `BUFFER`, `CURSOR`, or `zle -N` function bodies.

Implementation status: Phase 16e is implemented on
`codex/zsh-compat-command-not-found`.

Phase 16e targets command-miss handlers that zsh plugins normally install by
sourcing platform-specific `command_not_found_handler` scripts.

Phase 16e adds a native `command-not-found` preset:

- recognize `plugins=(command-not-found)` as a native dynamic plugin candidate
  even when the Oh My Zsh plugin directory is not installed locally.
- suggest a disabled `[zsh.native_plugins]` import-plan block with
  `presets = ["command-not-found"]`.
- when explicitly enabled, intercept rubash `CommandNotFound` errors in the
  winuxsh host layer and print Windows-native package-manager search hints.
- detect available `winget`, `scoop`, and `choco` shims through the same
  Windows PATH/PATHEXT resolver used by native plugin commands.
- keep the first implementation read-only: suggest `winget search`,
  `scoop search`, or `choco search`, but do not automatically run package
  manager searches during command execution.

Rules:

- disabled by default and never enabled only because `.zshrc` mentions the
  plugin.
- no Oh My Zsh `command-not-found.plugin.zsh` sourcing and no Linux/macOS
  platform handler sourcing.
- keep command execution deterministic for agents: missing commands still
  return 127 and package manager hints are advisory only.

Implementation status: Phase 16f is implemented on
`codex/zsh-compat-fzf-selector`.

Phase 16f targets interactive selector plugins that usually install ZLE
widgets and keybindings around `fzf`.

Phase 16f adds native `fzf` / `zsh-interactive-cd` selector presets:

- recognize `plugins=(fzf)` and `plugins=(zsh-interactive-cd)` as native
  dynamic plugin candidates even when the Oh My Zsh plugin directories are not
  installed locally.
- suggest a disabled `[zsh.native_plugins]` import-plan block with
  `presets = ["fzf"]` and/or `presets = ["zsh-interactive-cd"]`.
- when explicitly enabled, provide native `cdf` and `fzf-cd` command shims
  that list shell-visible subdirectories, pipe them to native `fzf`, and then
  `cd` through rubash to the selected path.
- keep `cd <Tab>`, Ctrl-T, Ctrl-R, Alt-C, and fzf's generated zsh keybinding
  scripts report-only until there is a tested reedline-native selector widget
  surface.

Rules:

- disabled by default and never enabled only because `.zshrc` mentions the
  plugin.
- no Oh My Zsh `fzf.plugin.zsh` / `zsh-interactive-cd.plugin.zsh` sourcing and
  no `eval "$(fzf --zsh)"`.
- selector commands may launch native `fzf` only after explicit
  `[zsh.native_plugins]` opt-in; non-interactive scripts remain unaffected.

Implementation status: Phase 16g is implemented on
`codex/zsh-compat-last-working-dir`.

Phase 16g targets stateful directory plugins that normally use `chpwd` to keep
per-user shell state across interactive sessions.

Phase 16g adds a native `last-working-dir` preset:

- recognize `plugins=(last-working-dir)` as a native dynamic plugin candidate
  even when the Oh My Zsh plugin directory is not installed locally.
- suggest a disabled `[zsh.native_plugins]` import-plan block with
  `presets = ["last-working-dir"]`.
- when explicitly enabled, provide a native `lwd` command shim that reads the
  cached shell-visible directory and `cd`s through rubash.
- when explicitly enabled in the interactive REPL, restore the cached directory
  once at startup only if the shell starts in the normal Windows home
  directory, matching the Oh My Zsh plugin's "do not steal an explicitly opened
  folder" behavior.
- update the cached directory from native `chpwd` lifecycle points.

Rules:

- disabled by default and never enabled only because `.zshrc` mentions the
  plugin.
- no Oh My Zsh `last-working-dir.plugin.zsh` sourcing.
- startup restore is REPL-only; `winuxsh -c ...` and script files must not
  automatically change directory.
- cache paths stay under `~/.winuxsh/cache/last-working-dir` (plus optional
  `SSH_USER` suffix) instead of requiring Oh My Zsh's `ZSH_CACHE_DIR`.

Implementation status: Phase 16h is implemented on
`codex/zsh-compat-dotenv`.

Phase 16h targets project-local environment plugins that normally source
`.env` during startup and `chpwd`.

Phase 16h adds a native `dotenv` preset:

- recognize `plugins=(dotenv)` as a native dynamic plugin candidate even when
  the Oh My Zsh plugin directory is not installed locally.
- suggest a disabled `[zsh.native_plugins]` import-plan block with
  `presets = ["dotenv"]`.
- when explicitly enabled, read the shell-visible current directory's `.env`
  from native `precmd` / `chpwd` lifecycle points and apply safe key/value
  assignments to rubash's environment.
- support simple `KEY=value` and `export KEY=value` records, including basic
  single/double quoted values.
- skip invalid variable names, oversized files, multiline shell constructs, and
  values containing command substitution markers such as `$(` or backticks.

Rules:

- disabled by default and never enabled only because `.zshrc` mentions the
  plugin.
- no Oh My Zsh `dotenv.plugin.zsh` sourcing and no direct `.env` sourcing.
- dotenv loading is REPL-only through native lifecycle hooks; `winuxsh -c ...`
  and script files must remain deterministic.
- first native pass does not implement OMZ's interactive allow/disallow prompt;
  opt-in `[zsh.native_plugins]` is the explicit trust boundary.

Verification notes:

- `plugins=(dotenv)` is classified as a native dynamic preset and import-plan
  suggests disabled `[zsh.native_plugins] presets = ["dotenv"]`.
- Native `precmd` and `chpwd` hook points load current-directory `.env` files
  only when the preset is explicitly enabled.
- `.env` parsing covers simple `KEY=value`, `export KEY=value`, quoted values,
  and comments while skipping unsafe keys such as `PATH` / `NODE_OPTIONS`,
  invalid names, command substitution markers, backticks, and oversized files.

## Windows-Native Host Contract Stabilization

Implementation status: completed on `codex/zsh-compat-dotenv`.

This follow-up keeps the zsh-like UX aligned with the Windows-native product
contract:

- after `cd`, winuxsh syncs rubash's shell-visible `PWD` back to the host
  process cwd so prompt rendering, completion cwd, and external commands agree.
- Windows shell-visible paths now normalize to `C:/...` instead of `/c/...`;
  `/c/...` remains accepted as a compatibility input.
- winuxcmd path-style commands translate legacy `/c/...` operands to
  `C:/...` before execution, so `ls /c/Users` works even though winuxcmd itself
  is Windows-native.
- command completion treats empty command position and partial command words
  correctly, so blank Tab lists commands and `gre<Tab>` can suggest `grep`.
- REPL single-line command sequences now reuse the same host-synced execution
  wrapper, so `cd target; native-child` updates process cwd before the native
  child runs.
- Phase 27 is complete: naked Windows drive paths such as `C:\Users\me` are
  recognized before rubash tokenization and normalized to `C:/Users/me`, so
  the bash lexer does not consume backslashes as escapes. This is a
  Windows-host input contract, not a zsh syntax extension.

## Phase 17 - Host Contract Test Matrix

Implementation status: completed on `master`.

This phase turns the Windows-native behavior fixed after Phase 16h into
binary-level regression tests. The tests should exercise the same `winuxsh -c`
surface used by agents and scripts, not only internal unit helpers.

Coverage target:

- `cwd`: `cd`, `pwd`, and a native Windows child process agree.
- `PATH`: executables discovered through child-process `PATH` can run.
- `env`: exported rubash variables reach native Windows child processes.
- `home`: `~` resolves to the normal Windows user home, not an isolated prefix.
- `stdout` / `stderr`: redirection keeps streams separated.
- `exit-code`: nonzero shell exit status becomes the process exit status.

Implementation notes:

- added a binary integration test suite for `winuxsh -c` host behavior.
- simple top-level scripts containing `cd` now execute through a conservative
  host-synced wrapper so `cd; pwd; native-child` agree on cwd immediately.
- `cd ~` is normalized before rubash execution so `~` remains the normal
  Windows home while visible `pwd` output stays `C:/...`.
- complex AST scripts still use rubash whole-file execution for heredoc,
  continuation, loops, functions, and other bash semantics.

Rules:

- do not change rubash parser/executor semantics.
- do not introduce MSYS2/Git Bash style path authority.
- keep `/c/...` as compatibility input only; default visible paths stay
  Windows-native `C:/...`.

## Phase 18 - REPL Completion Probe

Implementation status: completed on `master`.

This phase makes the interactive completion surface testable without a TTY. The
goal is to catch regressions like blank Tab returning zero rows or `gre<Tab>`
not suggesting `grep`, while still exercising the same `Shell::new()`
initialization path that the REPL uses.

Implemented interface:

- `winuxsh --completion-probe <line> [cursor]`.
- builds a real `Shell`, syncs completion state, runs the native completer, and
  print one completion value per line.
- keeps the command deterministic and non-interactive so agents and CI can use
  it.

Coverage target:

- empty command line suggests builtins / winuxcmd commands such as `ls`.
- partial command words such as `gre` suggest `grep`.
- commands discovered through Windows `PATH` / `PATHEXT` are suggested.
- command position after a pipe such as `ls | gre` suggests `grep`.
- argument position such as `echo gre` does not suggest command names.

Rules:

- do not rely on piping into reedline for tests.
- do not change rubash execution semantics.
- keep completion providers ordered the same way as interactive REPL startup.

## Phase 19 - Blank Argument Path Completion

Implementation status: completed on `master`.

This phase fixes a basic zsh-like completion behavior uncovered by the Phase 18
probe: `cd <Tab>` and `ls <Tab>` should offer current-directory path
candidates even when the user has not typed the first character of the
argument. Empty command position should still offer commands, not paths.

Coverage target:

- `winuxsh --completion-probe "ls "` suggests files and directories from the
  current directory.
- `winuxsh --completion-probe "cd "` suggests current-directory path
  candidates, with directories marked using a trailing slash.
- `winuxsh --completion-probe "ls | "` stays in command-position completion.
- `winuxsh --completion-probe "echo gre"` still must not suggest `grep` as a
  command argument.

Rules:

- do not special-case PowerShell wildcard behavior.
- keep path completion Windows-native and relative to the synchronized host cwd.
- do not change rubash parsing or command execution.

## Phase 20 - Path Completion Polish

Implementation status: completed on `master`.

This phase closes several path-completion gaps exposed by the non-interactive
probe. The most important correctness fix is preserving the typed directory
prefix: completing `src/ma` must produce `src/main.rs`, not `main.rs`.

Coverage target:

- `winuxsh --completion-probe "ls parent/"` keeps the `parent/` prefix in
  returned candidates.
- `winuxsh --completion-probe "ls parent/ch"` returns `parent/child.txt`.
- paths containing spaces are shell-escaped, e.g. `two\ words.txt`.
- hidden files are not shown for blank `ls `, but are shown when the typed
  prefix starts with `.`.
- directory candidates sort before file candidates.

Rules:

- keep candidates shell-visible and slash-style where the user typed slash
  prefixes.
- do not introduce PowerShell wildcard behavior.
- do not change rubash parser/executor semantics.

## Phase 21 - Shell-Word-Aware Completion

Implementation status: completed on `master`.

This phase makes the completion frontend understand the shell word currently
under the cursor instead of splitting only on raw whitespace. It is still a
completion-layer parser only; rubash remains the sole shell parser/executor.

Coverage target:

- `winuxsh --completion-probe "ls two\ w"` matches a file named
  `two words.txt`.
- `winuxsh --completion-probe "ls \"two w"` matches and returns a quoted
  candidate such as `"two words.txt"`.
- `winuxsh --completion-probe "ls parent\ dir/ch"` preserves the escaped
  parent directory prefix.
- suggestion spans replace the full shell word, not only the suffix after an
  escaped space.
- command-position and previous-token detection continue to work for unquoted
  commands and flags.

Rules:

- support only practical shell-word parsing needed by completion: backslash
  escapes, single quotes, double quotes, and command separators outside quotes.
- do not implement zsh-specific syntax.
- do not change rubash parser/executor semantics.

## Phase 22 - Prompt Indicator Polish

Implementation status: completed on `master`.

This phase adds a small native prompt indicator surface for zsh-like editor
feedback without changing shell semantics. It is intentionally a prompt/config
feature: `.zshrc` import can later target these fields, but winuxsh still does
not source zsh prompt code or ZLE widgets.

Coverage target:

- `[shell] prompt_indicator` configures the default / emacs indicator.
- `[shell] emacs_indicator`, `vi_insert_indicator`, and
  `vi_normal_indicator` can override editor-mode-specific indicators.
- `[shell] multiline_indicator` configures continuation prompts.
- `[shell] history_search_indicator` and `history_search_fail_indicator`
  configure Ctrl+R prompt text and can include `{term}` / `{status}`.
- omitted indicator fields preserve current behavior.

Rules:

- keep indicators as native reedline prompt rendering.
- do not execute zsh prompt functions or ZLE widgets.
- do not change rubash parser/executor semantics.

## Phase 23 - History Config Polish

Implementation status: completed on `master`.

This phase turns the existing fixed reedline history setup into a small native
configuration surface that matches the zsh-like product goal without adopting a
large history subsystem or Nushell semantics.

Target config:

```toml
[history]
path = "~/.winuxsh_history"
max_size = 10000
ignore_space_prefixed = true
```

Coverage target:

- omitted `[history]` preserves the current defaults:
  `~/.winuxsh_history`, capacity `10000`, and no ignore-space filtering.
- `path` supports `~` expansion to the normal Windows user home.
- `max_size` controls `FileBackedHistory::with_file(...)` capacity.
- `ignore_space_prefixed` maps to reedline's native history exclusion prefix
  for commands that start with a single space.

Rules:

- keep history as a native winuxsh/reedline feature.
- do not source zsh history scripts or make zsh `HISTFILE` authoritative yet.
- do not introduce SQLite/history isolation in this phase.
- keep non-interactive `-c` and script execution unaffected.

## Phase 24 - Completion UX Config

Implementation status: completed on `master`.

This phase adds a small native completion behavior surface inspired by zsh
matcher ergonomics and modern shell config. It should not implement zsh
`matcher-list` wholesale yet; it should expose the most useful controls needed
for daily Windows-native tab completion.

Target config:

```toml
[completions]
case_sensitive = false
matching = "prefix" # prefix | substring
max_command_results = 500
```

Coverage target:

- omitted fields preserve the current behavior: case-insensitive prefix
  matching with no command-result cap.
- `matching = "substring"` lets command/path/value completion match candidates
  that contain the typed word, not only candidates that start with it.
- `case_sensitive = true` makes matching respect the typed case.
- `max_command_results` caps command-position candidates so blank Tab stays
  responsive on large Windows PATHs.

Rules:

- keep command execution and rubash semantics unchanged.
- do not implement fuzzy matching or zsh matcher-list in this phase.
- keep user TOML completion definitions and zsh-imported definitions on the
  existing priority path.

Implemented:

- `[completions].case_sensitive` controls command/path/flag/value matching case.
- `[completions].matching` supports `prefix` and `substring`, with unknown values
  falling back to `prefix`.
- `[completions].max_command_results` caps command-position suggestions without
  changing path or argument completion.
- `WINUXSH_CONFIG` can point tests or advanced wrappers at an explicit config
  file; the default remains `~/.winshrc.toml`.

Verification:

- `cargo fmt --check`
- `cargo test --lib -p winuxsh-runtime --locked`
- `cargo test -p winuxsh-runtime --test completion --locked`
- `cargo test --test completion_probe --locked`
- `cargo test -p winuxsh-runtime --test zsh_compat --locked`
- `cargo test --test host_contract --locked`
- `cargo build --locked`
- `cargo test --test compat --locked -- --ignored`

## Phase 25 - Reedline Menu UX Config

Implementation status: completed on `master`.

This phase adds a small native menu behavior surface for the fixed completion
and history menus. It should make daily Tab/Ctrl+R interaction easier to tune
without introducing a zstyle-compatible menu DSL.

Target config:

```toml
[menus]
completion_page_size = 10
history_page_size = 10
max_entry_lines = 5
```

Coverage target:

- omitted fields preserve reedline's current ListMenu defaults.
- zero values fall back to defaults instead of creating unusable menus.
- completion and history menus can be sized independently.
- max entry lines applies to both menus to keep very long suggestions readable.

Rules:

- keep Tab completion and Ctrl+R history search keybindings unchanged.
- do not implement zstyle menu/select/group/order translation in this phase.
- do not change command execution, rubash semantics, or completion matching.

Implemented:

- `MenuConfig` stores completion/history page size and max entry line limits.
- `[menus]` parses `completion_page_size`, `history_page_size`, and
  `max_entry_lines`.
- zero or invalid values fall back to reedline-compatible defaults.
- `repl.rs` applies the config when constructing completion and history
  `ListMenu` instances.

Verification:

- `cargo fmt --check`
- `cargo test --lib -p winuxsh-runtime --locked`
- `cargo test -p winuxsh-runtime --test completion --locked`
- `cargo test --test completion_probe --locked`
- `cargo test -p winuxsh-runtime --test zsh_compat --locked`
- `cargo test --test host_contract --locked`
- `cargo build --locked`
- `cargo test --test compat --locked -- --ignored`

## Phase 26 - Standard ZLE Bindkey Subset

Implementation status: completed on `master`.

This phase maps a conservative subset of standard zsh ZLE widget names to
reedline native events when those widgets appear in safe `bindkey KEY WIDGET`
records. It should make common `.zshrc` key customizations useful without
executing arbitrary ZLE function bodies or introducing a keybinding DSL.

Target examples:

```zsh
bindkey '^A' beginning-of-line
bindkey '^E' end-of-line
bindkey '^[b' backward-word
bindkey '^[f' forward-word
bindkey '^K' kill-line
bindkey '^L' clear-screen
```

Coverage target:

- standard built-in ZLE widgets map to reedline edit events.
- supported standard bindkeys do not emit the old generic unsupported bindkey
  diagnostic.
- import-plan emits `[zsh.native_widgets] enabled = false` with
  `import_bindkeys = true` for standard bindkey-only profiles.
- custom ZLE widgets remain review-only unless already mapped to a known native
  winuxsh widget.

Rules:

- do not source zsh functions or implement ZLE scripting.
- do not override native reedline defaults unless the user explicitly enables
  `[zsh.native_widgets]`.
- keep Tab completion and Ctrl+R history search preserved.

Implemented:

- standard ZLE widgets such as `beginning-of-line`, `end-of-line`,
  `backward-word`, `forward-word`, `kill-line`, `clear-screen`, and
  `accept-line` map to reedline native events.
- zsh Esc/Alt key spellings such as `^[b` and `\\ef` are parsed for bindkey
  import.
- supported standard bindkeys no longer emit the generic unsupported bindkey
  diagnostic.
- standard bindkey-only profiles get a reviewable `[zsh.native_widgets]`
  import-plan entry with `enabled = false` and `import_bindkeys = true`.

Verification:

- `cargo fmt --check`
- `cargo test --lib -p winuxsh-runtime --locked`
- `cargo test -p winuxsh-runtime --test zsh_compat --locked`
- `cargo test -p winuxsh-runtime --test completion --locked`
- `cargo test --test completion_probe --locked`
- `cargo test --test host_contract --locked`
- `cargo build --locked`
- `cargo test --test compat --locked -- --ignored`

## Phase 28 - Interactive Multiline Collector

Implementation status: completed on `master`.

This phase closes the REPL gap where pasted or manually entered compound
commands were executed one physical line at a time. Script files and `-c`
already use whole-script execution; the missing piece is an interactive
frontend buffer that waits until the user has entered a complete shell block.

Implemented:

- REPL now keeps a pending input buffer across physical lines.
- pending input uses the configured multiline indicator as a PS2-style prompt.
- complete multi-line input is executed once through `execute_script`, while
  single-line commands continue through the existing `execute_line` path for
  native command shims.
- Ctrl+C clears pending input; Ctrl+D at PS2 cancels the pending block and
  returns to the primary prompt.
- Follow-up: shell comments are ignored by the completeness scanner, so pasted
  headings such as `# 11. 条件判断 (if)` do not start a phantom `if` block.

Coverage target:

- `if ... then` waits for `fi` and executes the full block once.
- `for` / `while` / `until` / `select` wait for `done`.
- `case` waits for `esac`.
- simple function definitions and brace groups wait for `}`.
- trailing `|`, `&&`, `||`, unclosed quotes, and backslash-newline keep the
  continuation prompt active.
- pending input uses the configured `[shell].multiline_indicator` / PS2-style
  prompt instead of the primary prompt.

Rules:

- keep rubash as the only parser/executor for the completed script.
- do not reintroduce the old winsh parser/core/ast.
- do not change non-interactive `-c` or script file execution behavior.

## Non-Goals

- Do not vendor zsh, Nushell, Oh My Zsh, or zsh plugin source into the winuxsh
  repository.
- Do not make `.zshrc` the only runtime config or execute it directly; TOML
  remains the native control plane and rollback-safe import target.
- Do not execute zsh plugin scripts during normal startup.
- Do not add a zsh parser/executor in winuxsh.
- Do not change `~` away from the normal Windows user home.

## Definition of Done

- A user with a simple Oh My Zsh `.zshrc` can run a winuxsh import command and
  get aliases, plugin completions, theme choice, edit mode, and zsh-like
  autosuggestions/highlighting without breaking startup.
- Unsupported zsh features are reported clearly.
- Non-interactive agent mode remains deterministic and quiet.
- No zsh/Nushell/Oh My Zsh source is vendored into the winuxsh repo.
