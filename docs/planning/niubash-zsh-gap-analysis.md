# Niubash gap analysis: reaching ZSH-grade power

Status: planning input for the post-1.0.1 roadmap, alongside
`niubash-v3-plan.md` and `plugin-ecosystem-vs-zsh.md`.
Scope: what actually makes zsh feel powerful, where niubash stands today, and
which gaps are worth closing in which order.

## Verdict

The language engine is **not** the gap. rubash already ships bash-grade
semantics (86/86 GNU upstream tests, arrays, assoc arrays, arithmetic,
process substitution, extglob/globstar, read/mapfile/getopts, select,
tilde/brace expansion). Chasing zsh-specific language surface (parameter
expansion flags, `zmodload`, zle-embedded syntax) would violate the v3
non-goal of "no second shell language runtime".

ZSH's real power comes from the **shell as a platform**: a programmable line
editor, user-authorable completions, lifecycle hooks, and an open ecosystem.
Measured on those pillars, one structural hole stands out.

## Pillar audit (evidence-based)

| Pillar | ZSH mechanism | Niubash today (code evidence) | Gap | Priority |
| --- | --- | --- | --- | --- |
| Programmable line editor | ZLE: `zle -N`, shell-code widgets, keymap stacks, fzf/autosuggestions/zsh-syntax-highlighting all ride on it | reedline (Rust library) provides editing, highlight, autosuggest, menus; repl.rs already imports bundle bindkey metadata (`NativeWidgetBinding`) into a **closed** widget-name mapping (`native_widget_event()`); unknown widget names are silently dropped. No user-level bind surface (`bind` builtin is a stub), no escape hatch to shell functions | Narrow: the missing 20% is a shell-function widget contract on rails reedline already ships | **P0** |
| Completion system | compsys: compdef functions in shell script, zstyle styles, menu selection with descriptions | `crates/niubash-runtime/src/completion/` (~4.5K lines): TOML defs + bash-completion import + `cmd -h` scrape + 3-level cache + path/variable/alias completion | No user-script completion functions, no zstyle-like config surface, menu selection quality unknown | **P1** |
| Lifecycle hooks | precmd/preexec/chpwd/periodic + add-zsh-hook | `config.rs` already defines `precmd`/`preexec`/`chpwd` arrays; plugin exports declare hook usage | Mostly done. Add periodic + a documented hook contract | Polish |
| Ecosystem openness | zero-metadata convention, any git repo is a plugin | Single official bundle (oh-my-niu); openness audit already written with P0-P3 roadmap | Execution gap, not design gap | **P2** |
| History & sessions | shared history, incremental search, history widgets | `NIU_HISTORY_MODE` shared/session/private, Ctrl+R search | Multi-line entries, timestamped search UI | P3 |
| Native superpowers | (ZSH has none of this) | Native paths, elevation builtin, Windows Terminal integration, self-update, 170 ms cold start | This is the moat, not a gap. Keep investing | Ongoing |

## P0 design: NIU widget contract (niubash-native, no ZSH compatibility)

Status: **implemented** (runtime 1.0.1). The contract below ships as-is:
`NIU_BINDKEYS` parsing, the `ExecuteHostCommand` widget escape hatch, and the
host widget executor with environment in/out parameters. The bundle bindkey
pipeline is also wired: `plugin_native_widget_bindings()` converts
`keybindings/*.toml` assets of enabled packs into real keymap bindings
(gated by the `keybindings` pack decision). User-facing docs live in
`docs/src/advanced-usage.md` ("Custom Key Widgets").

P1 is **implemented** on the same rails: `NIU_COMPDEFS` maps commands to
shell functions that run in the engine during completion through a
main-thread shell bridge (`Rc<RefCell<Shell>>` installed by the REPL;
reedline completers must stay `Send`). Contract: `NIU_COMP_WORDS` /
`NIU_COMP_CWORD` in, `NIU_COMP_RESULT` (one `value` or `value<TAB>description`
per line) out. See `docs/src/advanced-usage.md` ("Shell-Function
Completions").

Niubash does not aim for ZSH compatibility (product decision). The goal is
narrower: let **shell functions act as line editor widgets**, using only what
reedline 0.33 already ships. Verified against the vendored source:

- `ReedlineEvent::ExecuteHostCommand(String)` exits the read loop with
  `EventStatus::Exits(Signal::Success(...))` while the editor **suspends with
  full state** (buffer, cursor, undo, menus) and restores it on re-entry
  (`reedline 0.33 src/engine.rs`: suspension state, resume path, event
  dispatch). Nushell drives its `executehostcall` keybinding event through
  exactly this mechanism, on this same library version line.
- `Reedline::current_buffer_contents()` reads the live buffer.
- `Reedline::run_edit_commands(&[EditCommand])` is the public mutation
  channel (insert, clear, cursor moves).

No reedline fork, no rubash changes, no parser/executor access.

### What already exists in niubash

`repl.rs` has the metadata plumbing: bundle bindkeys (`NativeWidgetBinding`:
key sequence + keymap + widget name) are parsed and mapped onto
`ReedlineEvent`s via `native_widget_event()` (autosuggest-accept,
history-substring-search, cursor motions, kills, menus). The hole: the
mapping is closed, unknown widget names are dropped, and there is no
user-level binding source.

### The missing 20%

1. **User-level bindings.** Parse a `NIU_BINDKEYS` array from `~/.niubashrc`
   (same shape as bundle bindkeys, e.g. `"Ctrl+X c:niu_cd_fuzzy"`), stored in
   `shell`, consumed by `build_line_editor` alongside bundle metadata.
2. **Escape hatch.** In `native_widget_event()`, an unknown widget name
   becomes `ReedlineEvent::ExecuteHostCommand("__niu_widget <name>")`
   instead of `None`.
3. **Host widget executor.** The repl loop intercepts
   `Signal::Success("__niu_widget ...")` before treating it as submitted
   input, runs the named shell function with environment in/out parameters,
   applies results via `run_edit_commands`, and re-enters `read_line()` (the
   suspended editor repaints the new buffer).

### The v1 contract (environment variables, pure bash authoring)

```bash
# in ~/.niubashrc
niu_cd_fuzzy() {
    local dir
    dir="$(fd -t d | fzf)" || return 0
    NIU_WIDGET_RESULT="cd -- $dir"   # write back the whole buffer
    NIU_WIDGET_ACCEPT=1              # then submit it
}
NIU_BINDKEYS=("Ctrl+X c:niu_cd_fuzzy")
```

- In: `NIU_WIDGET_BUFFER` (current buffer), `NIU_WIDGET_CURSOR` (byte offset).
- Out: `NIU_WIDGET_RESULT` (replacement buffer; empty = no-op),
  `NIU_WIDGET_ACCEPT=1` (submit after applying).
- Insert-at-cursor is string surgery on `NIU_WIDGET_BUFFER` by the function
  itself; richer primitives (menus, multi-buffer) come later via reedline's
  Menu API if wanted.

### Why not clone ZLE

Post-zsh shells converged on "editor events + narrow host API"; none cloned
ZLE's interpreter-coupled model:

| Shell | Widget model | Coupling |
| --- | --- | --- |
| zsh | ZLE widget functions + `zle` primitives + `BUFFER`/`CURSOR` globals | deepest; compsys rides on it; nobody cloned it since |
| fish | `bind` -> fish function + `commandline` builtin (read/write buffer) | builtin-coupled, simpler than ZLE |
| nushell | declarative keybindings -> ReedlineEvent; `executehostcall` runs nu code mid-edit via reedline suspend/resume | host API only; same library niubash already uses |
| PowerShell | PSReadLine `-ScriptBlock` handlers + `GetBufferState`/`SetBufferState` | host API object |
| elvish | `edit:binding[Ctrl-X] = { ... }` lambdas per mode | in-language editor namespace |

The lesson matches the v3 non-goals: a narrow host contract delivers the
plugin-shaped power (fzf integration, buffer scripts, custom keys) without
embedding the editor into the interpreter.

### Sizing

`repl.rs` widget-event pass-through (~50 lines), `shell.rs` widget executor
(~120), `config.rs` `NIU_BINDKEYS` parsing (~60), keybinding plumbing already
exists. Tests mirror the existing `native_widget_*` tests in `repl.rs`.
`rubash`'s `bind` stub stays untouched: its "line editing not enabled"
behavior is correct bash script semantics; the interactive surface is
host-owned.

## P1: user-level completion contract

- `compdef <function> <cmd...>`: host calls the function with
  `NIU_COMP_WORDS` / `NIU_COMP_CWORD`, candidates come back as
  `value<TAB>description` lines on stdout.
- Menu selection upgrade in reedline: multi-column, descriptions, type-ahead
  filter, fuzzy matching (bonus for Chinese users: pinyin-insensitive path
  matching).
- A zstyle-analog: a few `NIU_COMPLETION_*` variables in `~/.niubashrc`
  (grouping, case sensitivity, cache TTL) instead of a new DSL.

## P2: execute the ecosystem openness roadmap

`plugin-ecosystem-vs-zsh.md` already specifies P0 (`niu plugin add
<git-url>[@ref]` + trust gate), P1 (federated indexes), P2 (classic
directory packs), P3 (read-only omz shim). No redesign needed; land P0.

## P3: polish list

- periodic hook + documented hook contract (`docs/src/hooks.md`).
- Multi-line history entries and timestamped Ctrl+R UI.
- did-you-mean on command-not-found stays suggestion-only (never auto-exec).
- Keep the cold-start budget: every new startup surface must stay inside the
  ~170 ms envelope (see `performance-investigation.md`).

## What we should NOT copy from zsh

- ZLE-embedded shell syntax and parameter-expansion flags (second language).
- `zmodload` module machinery (manifest-backed plugin registry already covers
  the lane, with a permission model zsh lacks).
- "Source anything" plugin trust defaults.

## Sizing note

P0 is reedline + host work (one yield primitive, one bind rewrite, one env
contract). P1 extends the existing completer registry. P2 is mostly
host-side plugin CLI on top of existing bundle install/update/rollback
machinery. None of these touch rubash language semantics.
