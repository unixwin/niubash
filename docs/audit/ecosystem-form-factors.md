# Ecosystem Plugin Form Factors — Evidence-Based Taxonomy

Lane: `wt94/ecosystem-forms` · 2026-10-02 · feeds #171/#172 (plugin manager), #183 (forms), #186 (TUI)

Owner question (2026-10-05): *"bash/zsh/fish 的插件生态是什么样的？只有 rc 吗？"*
Answer: **no — "source it in rc" is one of at least 13 distinct plugin form
factors.** This document enumerates them from real artifacts (shallow clones,
commit-pinned below), states what each form requires from the shell, and what
niu supports today. It is an audit only; **no product changes**.

## Method

16 upstream repos cloned shallow via proxy into a scratch area (NOT in the
repo): `/d/repo/_ecforms-refs/`. Every claim below cites `repo:path:line`
read from those clones at the pinned commits:

| Repo | Pin | Repo | Pin |
|---|---|---|---|
| ohmyzsh | `4d4cfc2` | bash-it | `4725d29` |
| zsh (zsh-users) | `8cc5ead` | oh-my-bash | `abf8461` |
| fish-shell | `35c2902` | basher | `7c3d238` |
| oh-my-fish | `043699a` | bpkg | `c7f5a46` |
| fisher | `791da64` | bash-completion | `00c3461` |
| zinit | `8682ebe` | fzf | `b1be3a8` |
| zplug | `cc6906e` | starship | `f5641f7` |
| direnv | `b00e451` | zoxide | `86b443c` |

niu-side citations are `engine:` for the rubash engine (`D:/repo/rubash`,
pinned by niubash's `[patch]` to the local worktree, 1.3.4 chain) and
`niu:` for `crates/niubash-runtime/`. Line numbers verified by grep on
2026-10-02.

---

## The taxonomy: 13 verified forms

### F01 — rc-sourced entry file (the classic)

A plugin is a file the shell `source`s at startup. Sub-forms:

- **F01a init-only monolith.** One file, self-contained. Examples:
  `bash-sensible` (`sensible.bash`, shipped in niu's `full` collection);
  `bash-preexec.sh` (rcaloras/bash-preexec — precmd/preexec hook layer, a
  niu recipe, `docs/plugins-guide.md:127`).
- **F01b framework plugin file selected by the manager's own loader.**
  oh-my-zsh: `oh-my-zsh.sh:316-317` (`_omz_source
  "plugins/$plugin/$plugin.plugin.zsh"`), 370 plugins, each a
  `*.plugin.zsh` of function + alias + `compdef` definitions (e.g.
  `plugins/git/git.plugin.zsh`). oh-my-bash: 37 `plugins/<n>/<n>.plugin.sh`
  pulled through `_omb_init_files` arrays (`oh-my-bash.sh:111-118`).
  bash-it: 81 `plugins/available/*.plugin.bash` activated by symlinks in
  `enabled/`, sourced via `scripts/reloader.bash` (`bash_it.sh:47-52`).
- **F01c metadata-annotated function file.** bash-it plugins carry composure
  doc metadata (`cite about-plugin` / `about:` / `param:` /
  `group:`, `plugins/available/base.plugin.bash:1-10`); the framework
  sources `vendor/github.com/erichs/composure/composure.sh` first
  (`bash_it.sh:21`).

**Load trigger:** startup, synchronously, in manager-defined order.
**Shell requirements:** `source`, functions, aliases, arrays; the manager
itself is just more sourced bash.
**niu status: WORKS — this is the driver's native model.** The managed rc
block emits one guarded source line per enabled asset
(`docs/plugins-guide.md` §three-layers); adapters exist for oh-my-bash rc
arrays, bash-it `enabled/`, bpkg trees, and wild `*.sh`/`*.bash`
(`niu: crates/niubash-runtime/src/plugins/{sources,spec,sync}.rs`).
Framework plugins fail exactly as GNU fails when sourced bare (no shims,
`docs/plugins-guide.md` §14.4). `expand_aliases` is on for `~/.niubashrc`
(`niu: src/shell.rs:538-544`), rc is `~/.niubashrc` (`niu: src/shell.rs:37`,
`:765-796`).

### F02 — function autoload directory (per-function files, first-call load)

A plugin ships a *directory* of files named after the functions they define;
the shell searches a search-path at **call time** and loads just the called
function.

- zsh: `fpath` entries + `autoload`. oh-my-zsh puts
  `$ZSH/{functions,completions}` and every enabled plugin dir on `fpath`
  before loading stock functions (`oh-my-zsh.sh:66-79, 88-94`). zsh itself
  ships `Functions/Zftp/zfcd`, `Functions/Prompts/prompt_*_setup`, etc.
  (one file per function).
- fish: `functions/*.fish` is the plugin substrate itself — 277 shipped
  files, e.g. `share/functions/trap.fish`. oh-my-fish documents the
  contract: *"Each function in your package must be declared in its own file
  under `functions` … required by fish autoloading mechanism, which loads
  functions on demand"* (`docs/en-US/Packages.md:51`).
- fisher copies plugin `functions/` into `$fisher_path/functions`
  (`functions/fisher.fish:165-170`).

**Load trigger:** first invocation of the function name (not startup).
**Shell requirements:** a function search path + per-name file lookup +
unknown-name deferral at parse time.
**niu status: MISSING.** No `fpath`/`autoload` concept exists in the engine
(grep: zero hits for `autoload`/`fpath` across `D:/repo/rubash/src`). bash
itself has no autoload either — the bash-ecosystem answer is F01/F03; the
gap matters if niu ever wants fish/zsh-layout trees to install without an
enumerate-and-source pass. Startup-cost lever: autoload defers work to first
call; niu's driver materializes eager source lines today.

### F03 — completion registry (file-per-command)

- **F03a eagerly sourced completion scripts.** bash-it
  `completion/available/*.completion.bash` (90 files), oh-my-bash
  `completions/`. Loaded at startup through the manager loop (F01).
- **F03b lazy per-command loader (bash-completion 2.17 layout).** A single
  default compspec `complete -D -F _comp_complete_load`
  (`bash-completion/bash_completion:3615-3617`); on first Tab for an unknown
  command it sources `$dir/completions/$cmd.bash` on demand
  (`:3548-3549`). 453 core + 88 fallback files in `completions-core/` +
  `completions-fallback/`; `completions/` is now the documented *external*
  drop-in dir ("loaded by bash-completion on demand",
  `completions/README.md`).
- **F03c zsh compdef registry.** Files whose first line is `#compdef cmd…`
  (e.g. `Completion/Unix/Command/_git:1`) are autoloaded through `compinit`
  into the `$_comps` association; 391 command files ship with zsh.
- **F03d fish `complete -c` files.** 1095 files, e.g.
  `share/completions/git.fish`, each a script of `complete -c cmd -s … -l …
  -a '…' -n 'condition'` registrations evaluated at query time.

**Load trigger:** startup (F03a) or first completion attempt (F03b/c/d).
**Shell requirements:** programmable completion: compspec registry, `-F`
function execution with `COMP_*`/`COMPREPLY`, `-C` command actions, a
**default compspec** (`complete -D`) consulted when the command has none,
and a retry after the loader sources the file.
**niu status: PARTIAL — the biggest functional hole.** The compspec registry
and static actions (wordlist/globpat/-W, `-X` filter, prefix/suffix) work
end-to-end into the REPL (`engine: src/builtins/complete.rs:897-927`; REPL
gates via `has_compspec`,
`niu: src/completion/completer.rs:244-258`; spec lookup
`engine: src/executor/public_accessors.rs:308-310`). But:
  - `-F`/`-C` dynamic actions are **not executed in the interactive path** —
    `complete_line_candidates` contributes only static actions
    (`engine: src/builtins/complete.rs:894` comment: "resolved by the
    executor and merged here in a follow-up";
    `engine: src/executor/public_accessors.rs:270-273`). `compgen -F func`
    does run and collect `COMPREPLY` in scripts
    (`engine: src/executor/job_builtins.rs:2364-2392`) — so the machinery
    exists but is not wired to Tab.
  - `complete -D/-E/-I` are parsed and stored under the pseudo keys
    (`engine: src/executor/job_builtins.rs:2042-2046`,
    `:2178-2186`) but **never consulted as fallbacks**
    (`engine: src/builtins/complete.rs:908-922` looks up only
    `specs.get(&command)`), so bash-completion's lazy loader
    (`complete -D -F _comp_complete_load`) can never fire.

### F04 — prompt/theme system

Theme files set `PS1`/`PROMPT` and per-prompt functions.

- oh-my-zsh: `themes/agnoster.zsh-theme:366-380` — `build_prompt()` +
  `PROMPT='%{%f%b%k%}$(build_prompt) '`; 143 themes; async prompt support
  (`lib/async_prompt.zsh`).
- bash-it: `themes/bobby/bobby.theme.bash:26-40` — builds `PS1` inside a
  `prompt_command` function and registers it with
  `safe_append_prompt_command` (PROMPT_COMMAND array append,
  `lib/preexec.bash:40`); 87 themes; theme resolution via `BASH_IT_THEME`
  (`bash_it.sh:58-74`).
- fish: `share/prompts/*.fish` (13 prompt functions, e.g. `default.fish`).
- Right-side prompt caveats documented in oh-my-bash's agnoster port
  (`themes/agnoster/agnoster.theme.sh:125-132`: bash has no RPROMPT;
  themes fake it with cursor surgery).

**Load trigger:** startup; re-evaluated every prompt.
**Shell requirements:** PS1/PROMPT re-expansion per prompt, prompt escapes,
array-capable `PROMPT_COMMAND`, enough PS1 escape support for themes to
render.
**niu status: WORKS — niu's flagship supported form.** Themes style
themselves through the PS1/PROMPT_COMMAND channel
(`niu: src/prompt.rs:47`, `:318-341` render path incl. right-align split);
PROMPT_COMMAND runs per-prompt, array-aware
(`engine: src/executor/prompt_command.rs:22-50`, eval.c:305 port). Theme
claim/pick is a first-class spec field with exclusive ownership
(`docs/plugins-guide.md` §theme ownership; `niu: src/plugins/theme_preview.rs`).

### F05 — key-binding pack

Files that install key-sequence → widget/shell-command bindings.

- oh-my-zsh: `lib/key-bindings.zsh:19-43` (`bindkey -M emacs "^[[A"
  up-line-or-beginning-search` …).
- fzf: `shell/key-bindings.bash` — `bind -x` packs that run shell commands
  inside the line editor (`:145-147` and the `__fzf_*` widgets above them);
  per-shell variants `key-bindings.{bash,zsh,fish}`.
- zoxide: `templates/bash.txt:169-199` — `bind -x '"\e[0n":
  __zoxide_z_complete_helper'` (OSC-7 completion trick).
- fish: `fish_default_key_bindings.fish:9-28` (`bind --preset …`),
  swappable `fish_vi_key_bindings` pack.

**Load trigger:** startup (or on demand).
**Shell requirements:** a real keymap the line editor consults; `bind -x`
executing a shell function with `READLINE_LINE`/`READLINE_POINT` in/out.
**niu status: MISSING (and silently so).** `bind` is a syntax-validating
stub: it parses options (`'m'|'f'|'q'|'u'|'r'|'x'` all "consume an argument"
and fall through to success) and **installs nothing**
(`engine: src/builtins/bind.rs:24-93`; no `READLINE_LINE`/`READLINE_POINT`
anywhere in the engine — grep zero hits). The interactive line editor is
reedline in the niu REPL with its own private widget map
(`niu: src/repl.rs:471`). Consequence: binding packs **load clean and do
nothing** — worse than an error, because `niu plugin enable fzf-git.sh`
(niu's own `recommended` collection entry, `docs/plugins-guide.md:129`)
appears to succeed while every binding is dead.

### F06 — startup drop-in directory (conf.d)

A directory the shell scans and runs at startup; plugins install by
*dropping files*, no central list edit.

- fish: `conf.d/*.fish` is a documented configuration/plugin seam; fisher
  copies plugin `conf.d/` into `$fisher_path/conf.d`
  (`functions/fisher.fish:165-170`) and warns that `--on-event` handlers
  *must* live there to be loaded when events fire
  (`fisher/README.md:135`).
- bash analogues: bash-it `custom/` + `enabled/` globs (`bash_it.sh:43-52,
  85-97`), oh-my-bash `custom/`.

**Load trigger:** every startup, file order.
**Shell requirements:** multi-directory startup scan. Nothing exotic.
**niu status: MISSING as a shell feature.** One primary rc
(`niu: src/shell.rs:37`, `:957` primary = `~/.niubashrc`), no conf.d scan
engine-side. The plugin driver *approximates* it by materializing a managed
rc block, but that is a generated list, not a drop-in seam, and per-file
enable (`niu plugin enable <id>/<file>`) is the only granularity.

### F07 — CLI-emitted integration snippet (the tool IS the plugin)

A standalone binary whose `init` subcommand *emits shell code* that the rc
`eval`s; the emitted code installs per-prompt hooks.

- starship: `src/init/starship.bash:1-2` — "We use PROMPT_COMMAND and the
  DEBUG trap to generate timing information… PROMPT_COMMAND is appended
  to"; defines `starship_preexec`/`starship_precmd` (`:19-33`), calls an
  optional user precmd hook (`:61-62`).
- direnv: `internal/cmd/shell_bash.go:19-23` — prepends `_direnv_hook` to
  `PROMPT_COMMAND` (array- vs string-aware, `declare -p` check).
- zoxide: `templates/bash.txt:65-72` — appends `__zoxide_hook` to
  `PROMPT_COMMAND` (bash-5.1 array branch + string branch); `eval
  "$(zoxide init bash)"` is the install form.
- basher: `libexec/basher-init` — `eval "$(basher init - bash)"` exports
  `BASHER_*` and prepends the cellar bin dir to PATH.

**Load trigger:** rc eval; the snippet's hooks fire per-prompt/per-exec
forever after.
**Shell requirements:** `eval "$(...)"` (command substitution of external
command), array-capable `PROMPT_COMMAND`, DEBUG trap firing before each
command, `trap` builtin.
**niu status: WORKS (with the F05 caveat).** PROMPT_COMMAND (array-aware)
and DEBUG/ERR/RETURN/EXIT traps are real
(`engine: src/executor/prompt_command.rs:22-50`;
`engine: src/builtins/trap.rs:520-522`, DEBUG firing sites across the
executor: `ast_exec.rs:494`, `pipeline_exec.rs:828/1076/1556/1617`,
`function_calls.rs:467`, `command_substitution.rs:725`). The product
explicitly expects `starship init` to live in `~/.niubashrc`
(`niu: src/shell.rs:538`) and the engine handles starship's
`${var:$((var="$(cmd)",0)):0}` idiom
(`engine: src/executor/parameter_core.rs:320`). PS0 is **not** expanded
(no `"PS0"` lookup — grep), so PS0-based integrations would lose that
channel; zoxide's `bind -x` piece dies with F05.

### F08 — event-handler functions (fish-native event system)

Functions registered against shell events.

- `--on-event fish_prompt / fish_preexec / fish_postexec / fish_exit /
  fish_focus_in / fish_read`: `share/functions/fish_vi_cursor.fish:23-27`.
- `--on-variable PWD`, `--on-variable fish_bind_mode`:
  `share/functions/__fish_config_interactive.fish:149`,
  `fish_vi_cursor.fish:23`.
- `--on-signal`: fish even ships a bash-`trap` compatibility *function*
  built on events — `share/functions/trap.fish:3-56` (exit path uses
  `--on-event fish_exit`, `:55`).
- fisher: plugin event handlers must be placed in `conf.d/` so they load
  when events are emitted (`fisher/README.md:135`).

**Load trigger:** function definition time registers the handler; fires on
the event.
**Shell requirements:** an event bus (named events, variable-watch, signal,
job-exit) + handler registration on definition.
**niu status: MISSING (no event bus).** Bash-family analogues cover only the
prompt cycle: PROMPT_COMMAND ≈ `fish_prompt`, DEBUG trap ≈ `fish_preexec`,
ERR trap (`engine: src/builtins/trap.rs:521`) ≈ an error-only postexec. A
fish plugin cannot be *loaded*, only *ported*; the plugin driver's job here
is taxonomy honesty (tag fish-layout trees as port-required), not
emulation.

### F09 — zsh machine forms (native modules, compiled caches)

- `zmodload` shared modules: zsh ships `Src/Modules/*.mdd` (attr, cap,
  clone, curses, datetime, db_gdbm, files, …) built to `zsh/*.so`; plugins
  depend on them (e.g. oh-my-zsh bgnotify: `zmodload zsh/datetime`,
  `plugins/bgnotify/bgnotify.plugin.zsh:8`).
- Compiled caches: `zrecompile` `.zwc` files and the `zcompdump` completion
  cache (`oh-my-zsh.sh:79, 215-256`).

**niu status: NOT APPLICABLE by design.** `enable -f` (bash loadable
builtin) loading is still refused by the engine — `engine:
src/builtins/enable.rs:101` TODO comment, `:153-181` "dynamic loading not
supported" diagnostics — and niu is preparing the *host* side separately
(the in-process readline C ABI export, niubash#775 / ruline#8). That work
exposes `rl_*` to bash builtins; it is not a zsh module ABI, so `.mdd`/`.so`
zsh modules and `.zwc` caches stay unsupported. The driver should
*recognize and skip* these artifacts (a `*.zwc` next to a `*.zsh` is cache,
not the plugin). The perf idea — caching expansion/completion state — is
niu's own problem to solve natively, not a compatibility form.

### F10 — lazy-load stubs (deferred loading)

- zinit turbo: `zinit ice wait"2"` loads the plugin after N seconds, with
  the manager installing stubs until then (`zinit/README.md:244-261`).
- oh-my-zsh nvm lazy: `zstyle ':omz:plugins:nvm' lazy` makes `nvm`/`node`/
  `npm`… shadow commands that load nvm on first use
  (`plugins/nvm/nvm.plugin.zsh:74-77`).

**Load trigger:** a timer (turbo) or first invocation of shadowed commands.
**Shell requirements:** for plugin-supplied stubs: none (they are plain
bash functions). For manager-side deferral: a scheduler or shadow-command
mechanism.
**niu status: PARTIAL.** Plugin-supplied lazy stubs are ordinary bash and
work (F01 semantics). The driver has no deferral concept — `plugins.toml`
has no `wait`/`defer` ice equivalent (`niu: src/plugins/spec.rs`) — and
startup cost is eager for everything enabled.

### F11 — PATH-integrated package (binaries + assets)

A plugin whose payload is executables plus per-shell asset files.

- basher: package layout linked into a cellar — `bin/`, `completions/`,
  `man/` symlinked by `bash-_link-bins` / `_link-completions` / `_link-man`;
  the bin dir joins PATH via `basher init`.
- bpkg: `bpkg.json`-described package with `"install": "bash setup.sh"`,
  `"commands"`, `"global"` (`bpkg/bpkg.json:1-16`); `bpkg install`
  fetches into a prefix.
- fisher: non-`.fish` files inside `functions/conf.d/completions` are
  copied through to `$fisher_path` (`fisher/README.md:129`).

**Load trigger:** PATH lookup at invocation; assets via their form.
**Shell requirements:** nothing (binaries); assets reduce to F01/F03.
**niu status: WORKS.** Binaries need only PATH; the sourcing side is F01.
Installation is deliberately out of scope (download retraction: tools come
from wpm/winget/scoop/apt/brew; `niu plugin add fzf` prints the commands —
`docs/plugins-guide.md` §executable tools).

### F12 — data-only assets (alias packs, abbreviations, theme vars)

- bash-it `aliases/available/*.aliases.bash` (50 packs); oh-my-bash
  `aliases/`.
- fish themes (config-var packs consumed by `fish_config theme`);
  fish abbreviations (`abbr`) as user-side data.
- zsh `lib/*.zsh` option/alias packs in oh-my-zsh.

**Load trigger:** startup source.
**Shell requirements:** aliases; abbreviations require an `abbr` subsystem.
**niu status: PARTIAL.** Alias packs work (aliases are core). Abbreviations
do not exist in the product (engine grep: only a doc comment,
`engine: src/executor/external_file_builtins.rs:1466`); a separate lane
(wt93/abbr) is carrying that feature — this audit only records the
dependency: abbreviation packs are a real asset form in the fish ecosystem.

### F13 — manager manifests and registries (the metadata form)

What managers record about installed plugins:

- fisher: the fishfile, one `owner/repo` per line
  (`fisher/README.md:56, 104-113`) + the implicit directory contract of
  F02/F03/F06/F08.
- bpkg: `bpkg.json` — name/version/repo/install/commands/global
  (`bpkg/bpkg.json:1-16`).
- zinit: inline "ices" attached to a load command —
  `zinit ice depth"1"`, `pick"async.zsh" src"pure.zsh"`, `as"command"
  from"gh-r"` (`zinit/README.md:206-220`); state in the `ZINIT_ICES`
  assoc (`zinit.zsh:17`).
- bash-it: `enabled/` symlink set + `BASH_IT_THEME` (`bash_it.sh:47-74`).
- (Contrast, already adopted by niu: lazy.nvim's spec + lockfile pair.)

**niu status: WORKS — spec parity exists.** `~/.niubash/plugins.toml`
declarative spec + `~/.niubash/sources/registry.toml` lock (commit + tree
checksum pins) + `niu plugin sync` reconciliation
(`docs/plugins-guide.md`; `niu: src/plugins/{spec,sync,trust}.rs`).
The gap is not metadata — it is that the spec's *enable* vocabulary only
addresses F01-style assets (see Gaps).

---

## Support matrix

| # | Form | niu status | Load-bearing engine cite |
|---|---|---|---|
| F01 | rc-sourced entry (+framework loops) | WORKS | `niu: src/plugins/sync.rs`; `engine: src/builtins/source.rs` |
| F02 | function autoload dir | MISSING | grep: no `fpath`/`autoload` in engine |
| F03 | completion registry | PARTIAL | `engine: src/builtins/complete.rs:897-927`; `-F/-C` not wired (`:894` + `public_accessors.rs:270-273`); no `-D` fallback |
| F04 | prompt/theme system | WORKS | `engine: src/executor/prompt_command.rs:22-50`; `niu: src/prompt.rs:47,318-341` |
| F05 | key-binding pack | MISSING | `engine: src/builtins/bind.rs:24-93` (validate-only stub) |
| F06 | startup drop-in dir (conf.d) | MISSING | `niu: src/shell.rs:37,957` (single rc) |
| F07 | CLI-emitted hook bootstrap | WORKS | `engine: trap.rs:520-522` + executor DEBUG sites; `niu: src/shell.rs:538` |
| F08 | event-handler functions (fish) | MISSING | no event bus (analogues: PROMPT_COMMAND/DEBUG/ERR only) |
| F09 | zsh modules/caches (.so/.zwc) | N/A (by design) | `engine: src/builtins/enable.rs:153-181` |
| F10 | lazy-load stubs | PARTIAL | plain-bash stubs work; no manager deferral (`niu: src/plugins/spec.rs`) |
| F11 | PATH-integrated package | WORKS | binaries need nothing; sourcing = F01 |
| F12 | data-only assets | PARTIAL | aliases yes; `abbr` missing (wt93/abbr lane) |
| F13 | manager manifest/lockfile | WORKS | `docs/plugins-guide.md`; `niu: src/plugins/{spec,sync,trust}.rs` |

**Counts: 13 forms — WORKS 6, PARTIAL 2, MISSING 4, N/A 1.**

Reading: the *bash* plugin world (F01/F04/F07/F11/F12/F13 — the six WORKS)
is where niu already plays; the PARTIAL/MISSING set is exactly the parts of
the ecosystem that outgrew "source it in rc": completion registries that
load lazily and run functions, keymaps, directory contracts, and events.

## Top 3 gaps (impact-ranked)

1. **F03 — interactive `-F`/`-C` completion execution + `complete -D`
   lazy-loader fallback.** The engine already runs completion functions for
   `compgen -F` (`job_builtins.rs:2364-2392`) and the REPL already gates on
   `has_compspec` (`niu: completer.rs:244-258`); what is missing is wiring
   `run_compgen_completion_function`-equivalent invocation into
   `complete_line_candidates` plus a `-D`-fallback lookup (pseudo key
   already stored, `job_builtins.rs:2042-2046`). Unblocks: bash-completion's
   541 per-command files (2.17 layout) and every `complete -F` snippet a CLI
   prints in its install instructions. This is the single change that turns
   "static wordlist completion" into the real programmable-completion form.
2. **F05 — `bind`/`bind -x` into the real line editor.** The stub
   (`bind.rs:24-93`) accepts everything and installs nothing, so binding
   packs fail *silently* — including `fzf-git.sh`, which niu's own
   `recommended` collection ships (`docs/plugins-guide.md:129`). Requires
   `bind` calls to mutate a keymap the reedline REPL consults, plus
   `bind -x` execution semantics (`READLINE_LINE`/`READLINE_POINT`). Until
   then the driver should *warn* when an enabled asset calls `bind` (the
   stub makes the plugin look alive when it is not).
3. **F02 + F06 — the directory contract (functions/ autoload dir, conf.d
   drop-ins).** The dominant fish/zsh plugin layout is a directory the
   shell picks up, not a file a manager sources; fisher's whole model
   (`functions/conf.d/completions` copy-in, README.md:117-135) and OMZ's
   fpath scan (`oh-my-zsh.sh:66-94`) are directory-shaped. niu's driver
   can only enumerate entry files and source them eagerly — which breaks
   autoload's first-call deferral (a startup-cost lever) and has no
   drop-in seam. Minimal bash-native move: a niu conf.d the rc sources
   with a glob, plus driver tags for layout kinds; full autoload/fpath is
   an engine feature and should be sized as such.

Runner-up worth one line: **PS0 is not expanded** (grep: no `"PS0"`
lookup) — a niche but real channel some integrations use; and
`command_not_found_handle` is absent, which some package-suggestion plugins
rely on.

## What the TUI must render/manage (#186)

| Form | TUI surface | Status today |
|---|---|---|
| F01 enable lists (per-file candidates with tags) | enable/disable views | EXISTS (`niu: src/plugins/ui.rs`, `sources.rs` candidate enumeration) |
| F04 theme gallery + exclusive theme claim | theme pick/preview | EXISTS (`niu: src/plugins/theme_preview.rs`; wizard post-install pick) |
| F13 spec/lock/trust state | trust review, sync rows, distro apply | EXISTS (`niu: src/plugins/{trust,spec,sync,distros}.rs`) |
| F03 completion registry | per-command enable/disable, "lazy-loaded on first Tab" hint | NEW (needs F03 engine work first) |
| F05 key-binding packs | pack cards + dead-binding warning until F05 lands | NEW (warning is cheap and honest now) |
| F06 conf.d drop-ins | directory toggles (enable whole dir, not single files) | NEW (needs F06) |
| F08 events / F09 modules | "not supported in niu" explainer rows (port-required / skip cache artifacts) | NEW (copy only) |
| F10 lazy stubs | defer/wait column in enable view | NEW (needs F10 manager support) |

Design consequence for #186: the TUI's unit of management must graduate
from "file to source" to "form-factor instance" — a theme, a completion
registry entry, a keymap pack, a conf.d drop-in, a manifest — because the
ecosystem's forms, not files, are what users pick from.

## Honesty notes

- All ecosystem line numbers were read from the pinned shallow clones in
  this session; niu/engine line numbers were grep-verified against the
  1.3.4-chain sources this worktree builds against.
- "WORKS" claims for F04/F07 rest on the cited code paths plus the product's
  documented starship/oh-my-bash tenancy (`docs/plugins-guide.md`,
  `niu: src/shell.rs:538`); no end-to-end behavioral runs were executed in
  this lane (audit-only, no product changes).
- Machine-readable companion: `docs/audit/ecosystem-forms.json` (same
  directory).
