# Built-ins & Fast Paths

Niubash resolves a command word through three layers, in priority order. This
page documents what lives in each layer, how the inventory is kept honest, and
where the inventory overlaps with WinuxCmd.

| Layer | What it is | Introspection (`type`, `command -v`, `enable`) |
| --- | --- | --- |
| Real built-in | Runs in-process, no child process. Listed in the rubash whitelist (`src/executor/builtin_names.rs`). | Reports as **builtin** |
| Fast path | Hidden optimization: runs in-process only for simple argument shapes; falls back to the external WinuxCmd command otherwise. Deliberately **not** in the whitelist. | Reports as **external** — same as GNU Bash, where these commands are external too |
| External command | Resolved via `PATH`, provided by WinuxCmd (176 commands). | Reports as external |

## Real built-ins (64)

All **61 GNU Bash 5.2 built-ins are covered — none missing**. The entries
below that are not part of Bash are deliberate extensions:

<!-- niubash-docs: builtins list (64). Do not edit without updating
     src/executor/builtin_names.rs in unixwin/rubash; a doc-sync test
     enforces the match. -->
<!-- builtins-list
.
:
[
alias
bg
bind
break
builtin
caller
cd
command
compgen
complete
compopt
continue
declare
dirs
disown
echo
enable
env
eval
exec
exit
export
false
fc
fg
getopts
hash
help
history
jobs
kill
let
local
logout
mapfile
popd
printf
pushd
pwd
read
readarray
readonly
return
set
setopt
shift
shopt
source
suspend
test
times
trap
true
type
typeset
ulimit
umask
unalias
unset
unsetopt
wait
-->

| Group | Commands |
| --- | --- |
| GNU Bash 61 built-ins (all aligned) | `.` `:` `[` `alias` `bg` `bind` `break` `builtin` `caller` `cd` `command` `compgen` `complete` `compopt` `continue` `declare` `dirs` `disown` `echo` `enable` `eval` `exec` `exit` `export` `false` `fc` `fg` `getopts` `hash` `help` `history` `jobs` `kill` `let` `local` `logout` `mapfile` `popd` `printf` `pushd` `pwd` `read` `readarray` `readonly` `return` `set` `shift` `shopt` `source` `suspend` `test` `times` `trap` `true` `type` `typeset` `ulimit` `umask` `unalias` `unset` `wait` |
| zsh compatibility extension | `setopt` `unsetopt` |
| In-process `env` | `env` (coreutils semantics, sorted output, `-0`) |
| Windows-only | `sudo` (not counted in the 64) |

Notes:

- `declare`/`typeset` and `source`/`.` are alias pairs; `[` is implemented by
  the `test` builtin and validates the closing `]`.
- `time`, `[[`, and `((` are **reserved words / grammar**, not built-ins —
  same as Bash. `time` honors `TIMEFORMAT`.
- The `sudo` built-in is a Windows-specific extension (`#[cfg(windows)]`).

## Fast paths (hidden built-ins)

These run in-process for simple argument shapes and fall back to the external
WinuxCmd command otherwise. They are intentionally excluded from the whitelist
so that introspection keeps reporting them as external, matching GNU Bash:

| Command | In-process behavior | Fallback |
| --- | --- | --- |
| `sleep` | Fractional seconds supported | external `sleep` |
| `dirname` | Plain paths | external `dirname` |
| `basename` | Plain paths | external `basename` |

`env` is the one hybrid: it **is** on the whitelist (reported as a built-in),
but its implementation is fully in-process with coreutils semantics and no
external fallback path.

Two additional reproduction built-ins exist for test compatibility only:
`recho` and `zecho` (mirrors of the helper built-ins in GNU Bash's own test
suite; not shipped in official Bash builds).

## Overlap with WinuxCmd

Thirteen command names exist both as rubash built-ins/reserved words and as
WinuxCmd external commands: `echo` `env` `kill` `printf` `pwd` `test` `[`
`true` `false` (real built-ins), `sleep` `dirname` `basename` (fast paths),
and `time` (reserved word).

The overlap follows Bash semantics: inside a script the built-in or reserved
word shadows the external command, while explicit forms such as
`command env` or `/usr/bin/env` still reach the WinuxCmd external version.
Both implementations were compared option-by-option (escapes, format
specifiers, signals, jobspec, `TIMEFORMAT`) on 2026-09-08 — aligned.

## How the inventory stays honest

The authoritative machine-readable lists live in
[`docs/builtins.md` on unixwin/rubash](https://github.com/unixwin/rubash/blob/master/docs/builtins.md).
A doc-sync test inside `src/executor/builtin_names.rs` parses that document
and fails `cargo test` whenever the whitelist or the fast-path dispatch
changes without the document being updated — and vice versa. Adding,
removing, or reclassifying a built-in therefore requires touching both the
code and the doc in the same change.
