# Niubash command quick reference

Data tables for the niubash agent skill. The tables between the GENERATED
markers are produced by `scripts/generate-skill.py` from the engine's own
surfaces (corpus transcripts of `niu --help` and `help -s '*'`, plus the
winuxcmd applet completion inventory) and are golden-checked by
`scripts/test-skill-bundle.py` — do not hand-edit inside the markers.
Everything outside them is curated.

Invocation shapes and exit-code contract:

```text
<!-- BEGIN GENERATED:launcher-verbs -->
Usage:  niu [GNU long option] [option] ...
        niu [GNU long option] [option] script-file ...
        niu -c <cmd>         Run a command then exit
        niu -C <cmd>         Run one REPL-style command then exit
        niu setup           Re-run prompt/plugin setup
        niu font            Nerd Font detection & install recommendations
        niu doctor          Health-check the installation
        niu <script> [args]  Run a script file

  self-update               REPL command: update Niubash and exit this shell
  update-niubash            Alias for self-update
<!-- END GENERATED:launcher-verbs -->
```

One-shot runs (`niu -c`, scripts) load no rc and no plugins; interactive
startup sources `~/.niubashrc` — preceded by the generated plugin loader
`~/.niubash/generated/plugins.niu` when the manifest produced one.
Non-interactive init files: `NIU_ENV` (higher precedence) or `BASH_ENV`.

Beyond the usage block, `niu --help` prints the full GNU invocation face
(the long-option and shell-option lists are generated from the engine's
own parse tables), plus these host-level verbs:

- Verbatim handoff — `niu --encoded-command <b64>` runs a base64 payload
  with zero quoting; `-EncodedCommand <b64>` (PowerShell spelling,
  case-insensitive) and `--ec <b64>` are the same flag. Prefix the payload
  with `v1:` when the command line might transit an MSYS-family rewriter.
- `--cwd-state <file>` — restore the recorded directory at startup and
  record the final directory back on exit (session continuity for hosts
  that relaunch the shell per command).
- `--install-wt-shortcuts [--update-only] [--quiet]` — rewrite
  desktop/Start-menu shortcuts into the `wt.exe -p Niubash` form.
- `niu config get|set|list` — persisted user settings in
  `~/.niubash/config.toml` (e.g. `completion.style`,
  `command-not-found-hint`).
- `niu skill install|status [--target codex|claude|zcode|cursor|all|<dir>]` —
  install or check this agent skill bundle itself.
- `niu plugin migrate-rc [--diff]` — read-only migration report for
  legacy managed rc blocks (also in the plugin verb table below).

Invoke the binary as `bash`/`sh` (the bundled hardlinks) for the
byte-for-byte GNU face: `bash --help`, `bash -c`, POSIX `sh` mode.

Plugin system verbs (git-clone-only external sources, explicit trust gate,
declarative spec `~/.niubash/plugins.toml` + lockfile
`~/.niubash/sources/registry.toml` driving the generated loader):

```text
<!-- BEGIN GENERATED:plugin-verbs -->
  plugin add <id|owner/repo|url|path>
                            Install an external plugin source (untrusted)
  plugin list [--json]      Sources, their assets, activation state
  plugin enable|disable <t> Activate or deactivate a source or asset
  plugin update|sync|restore|rollback|clean
                            Lockfile verbs (vim-plug/lazy.nvim-style)
  plugin trust <id>         Review and activate a source's assets
  plugin migrate-rc [--diff]
                            Read-only migration report for legacy managed
                            blocks (never writes the rc)
  plugin discover [--verbose]
                            Read-only overview of external plugin sources
  plugin source <command>   Full source protocol (add/trust/sign/verify/
                            remove/update/rollback/list)
  plugin recipe <command>   Recipe index (list/show/add)
  plugin distro <command>   Collections (list/import/remove/apply)
  plugin mirror <command>   Git fetch mirroring (list/show/set)
  plugin ui                 Menu UI (sections by state, same verbs)
<!-- END GENERATED:plugin-verbs -->
```

Deeper plugin protocol docs: `docs/plugins-guide.md` in the repository
(mirrored guides: `niu plugin --help` per subcommand).

## Shell built-ins (GNU bash 5.3 table)

`help <name>` prints the full page; `help -s '*'` prints these synopses.

<!-- BEGIN GENERATED:builtin-table -->
| Command | Synopsis |
| --- | --- |
| `!` | ! PIPELINE |
| `%` | job_spec [&] |
| `(( ... ))` | (( expression )) |
| `.` | . [-p path] filename [arguments] |
| `:` | : |
| `[` | [ arg... ] |
| `[[ ... ]]` | [[ expression ]] |
| `alias` | alias [-p] [name[=value] ... ] |
| `bg` | bg [job_spec ...] |
| `bind` | bind [-lpsvPSVX] [-m keymap] [-f filename] [-q name] [-u name] [-r keyseq] [-x keyseq:shell-command] [keyseq:readline-function or readline-command] |
| `break` | break [n] |
| `builtin` | builtin [shell-builtin [arg ...]] |
| `caller` | caller [expr] |
| `case` | case WORD in [PATTERN [\| PATTERN]...) COMMANDS ;;]... esac |
| `cd` | cd [-L\|[-P [-e]]] [-@] [dir] |
| `command` | command [-pVv] command [arg ...] |
| `compgen` | compgen [-V varname] [-abcdefgjksuv] [-o option] [-A action] [-G globpat] [-W wordlist] [-F function] [-C command] [-X filterpat] [-P prefix] [-S suffix] [word] |
| `complete` | complete [-abcdefgjksuv] [-pr] [-DEI] [-o option] [-A action] [-G globpat] [-W wordlist] [-F function] [-C command] [-X filterpat] [-P prefix] [-S suffix] [name ...] |
| `compopt` | compopt [-o\|+o option] [-DEI] [name ...] |
| `continue` | continue [n] |
| `coproc` | coproc [NAME] command [redirections] |
| `declare` | declare [-aAfFgiIlnrtux] [name[=value] ...] or declare -p [-aAfFilnrtux] [name ...] |
| `dirs` | dirs [-clpv] [+N] [-N] |
| `disown` | disown [-h] [-ar] [jobspec ... \| pid ...] |
| `echo` | echo [-neE] [arg ...] |
| `enable` | enable [-a] [-dnps] [-f filename] [name ...] |
| `eval` | eval [arg ...] |
| `exec` | exec [-cl] [-a name] [command [argument ...]] [redirection ...] |
| `exit` | exit [n] |
| `export` | export [-fn] [name[=value] ...] or export -p [-f] |
| `false` | false |
| `fc` | fc [-e ename] [-lnr] [first] [last] or fc -s [pat=rep] [command] |
| `fg` | fg [job_spec] |
| `for` | for NAME [in WORDS ... ] ; do COMMANDS; done |
| `for ((` | for (( exp1; exp2; exp3 )); do COMMANDS; done |
| `function` | function name { COMMANDS ; } or name () { COMMANDS ; } |
| `getopts` | getopts optstring name [arg ...] |
| `hash` | hash [-lr] [-p pathname] [-dt] [name ...] |
| `help` | help [-dms] [pattern ...] |
| `history` | history [-c] [-d offset] [n] or history -anrw [filename] or history -ps arg [arg...] |
| `if` | if COMMANDS; then COMMANDS; [ elif COMMANDS; then COMMANDS; ]... [ else COMMANDS; ] fi |
| `jobs` | jobs [-lnprs] [jobspec ...] or jobs -x command [args] |
| `kill` | kill [-s sigspec \| -n signum \| -sigspec] pid \| jobspec ... or kill -l [sigspec] |
| `let` | let arg [arg ...] |
| `local` | local [option] name[=value] ... |
| `logout` | logout [n] |
| `mapfile` | mapfile [-d delim] [-n count] [-O origin] [-s count] [-t] [-u fd] [-C callback] [-c quantum] [array] |
| `popd` | popd [-n] [+N \| -N] |
| `printf` | printf [-v var] format [arguments] |
| `pushd` | pushd [-n] [+N \| -N \| dir] |
| `pwd` | pwd [-LP] |
| `read` | read [-Eers] [-a array] [-d delim] [-i text] [-n nchars] [-N nchars] [-p prompt] [-t timeout] [-u fd] [name ...] |
| `readarray` | readarray [-d delim] [-n count] [-O origin] [-s count] [-t] [-u fd] [-C callback] [-c quantum] [array] |
| `readonly` | readonly [-aAf] [name[=value] ...] or readonly -p |
| `return` | return [n] |
| `select` | select NAME [in WORDS ... ;] do COMMANDS; done |
| `set` | set [-abefhkmnptuvxBCEHPT] [-o option-name] [--] [-] [arg ...] |
| `shift` | shift [n] |
| `shopt` | shopt [-pqsu] [-o] [optname ...] |
| `source` | source [-p path] filename [arguments] |
| `suspend` | suspend [-f] |
| `test` | test [expr] |
| `time` | time [-p] pipeline |
| `times` | times |
| `trap` | trap [-Plp] [[action] signal_spec ...] |
| `true` | true |
| `type` | type [-afptP] name [name ...] |
| `typeset` | typeset [-aAfFgiIlnrtux] name[=value] ... or typeset -p [-aAfFilnrtux] [name ...] |
| `ulimit` | ulimit [-SHabcdefiklmnpqrstuvxPRT] [limit] |
| `umask` | umask [-p] [-S] [mode] |
| `unalias` | unalias [-a] name [name ...] |
| `unset` | unset [-f] [-v] [-n] [name ...] |
| `until` | until COMMANDS; do COMMANDS-2; done |
| `variables` | variables - Names and meanings of some shell variables |
| `wait` | wait [-fn] [-p var] [id ...] |
| `while` | while COMMANDS; do COMMANDS-2; done |
| `{ ... }` | { COMMANDS ; } |
<!-- END GENERATED:builtin-table -->

## WinuxCmd applets on PATH

Real Windows binaries provided by the bundled WinuxCmd command layer
(`winuxcmd --version` for the runtime version, `wpm` to add more). Each
applet has embedded tab-completions generated from its `--help`.

```text
<!-- BEGIN GENERATED:applets -->
[ arch awk b2sum base32 base64 basename basenc cal cat chattr chcon chgrp chmod chown
chroot cksum clear cmp col column comm cp cpio csplit cut cygpath d2u date dd df diff
diff3 dir dircolors dirname dos2unix du echo egrep env envsubst expand expr factor false
fgrep file find fmt fold free gawk getconf getfacl getopt grep groups head hexdump
hmac256 hostid hostname id infocmp install join kill killall ldd less ln locale locate
logger logname look ls lsattr lsof man md5sum mkdir mkfifo mkgroup mknod mkpasswd mktemp
more mpicalc mv namei nice nl nohup nproc numfmt od paste patch pathchk pgrep pidof
pinky pkill pldd pr printenv printf ps ptx pwd readlink realpath regtool renice reset
rev rm rmdir runcon sdiff sed seq sha1sum sha224sum sha256sum sha384sum sha512sum shred
shuf sleep sort split stat stdbuf strings stty sum sync tac tail tee test tic time
timeout toe top touch tput tr tree true truncate tsort tty tzset u2d uname unexpand uniq
unix2dos unlink updatedb uptime users vdir vmstat w watch wc whereis which who whoami
wpm xargs xxd yes
<!-- END GENERATED:applets -->
```

## Package layer (wpm) — curated

```bash
wpm installed              # packages present in this WinuxCmd root
wpm list                   # full catalog with install state
wpm search NAME            # find a package
wpm info NAME              # package details
wpm install NAME           # install a missing command
wpm links list             # inspect command links
wpm links rebuild --force  # repair links after a broken update
wpm index update           # refresh the package index
wpm update winuxcmd        # update the command layer itself
```

`niu --self-update` (the shell) and `wpm update winuxcmd` (the commands)
are separate update planes.
