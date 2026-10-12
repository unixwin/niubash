# External Completion Providers

Niubash ships no third-party completion provider in its binary. The shell
provides the *protocol*; providers are *data* you source from `~/.niubashrc`
— the same setup a bash or zsh user writes by hand. Any tool that speaks the
bash completion protocol integrates this way; carapace-bin below is one
example, not a special case.

## The protocol surfaces

Three surfaces accept external completion code, all configured from the
startup rc:

### 1. `complete` compspecs (the GNU builtin)

The engine implements the `complete` builtin, so standard completion
registrations work as-is:

```bash
complete -W 'status switch merge rebase' mytool   # static wordlist
complete -f tar                                    # any file
complete -d cd                                     # directories only
```

Static compspecs serve candidates at Tab directly from the engine.
Dynamic hooks — `complete -F func` and `complete -C command` — register and
display correctly (`complete -p`), and `-F` functions execute inside scripts
through `compgen -F`; their resolution *at the interactive Tab press* is an
engine follow-up (see the rubash release notes). Until it lands, a
function-based provider should also speak the compdef protocol below.

### 2. compdef functions (`NIU_COMPDEFS`)

For providers that compute candidates with a shell function, the rc declares
a `command:function` mapping and defines the function. The function sees
`NIU_COMP_WORDS` (space-joined, bash COMP_WORDS semantics) and
`NIU_COMP_CWORD`; it writes back `NIU_COMP_RESULT`, one candidate per line,
as `value` or `value<TAB>description` — the description shows up in the
completion menu.

```bash
_mytool_comp() {
    # a real provider would shell out here and translate its output;
    # carapace's `value<TAB>description` fish output drops in unchanged
    NIU_COMP_RESULT="start"$'\n'"stop"$'\n'"restart\tRestart the service"
}
NIU_COMPDEFS="mytool:_mytool_comp"
```

Candidates are filtered by the typed prefix; `NIU_COMPDEFS` holds one
`command:function` pair per line.

### 3. Completion definition files

`<cmd>.toml` (flags, subcommands, value sources — the richest surface) and
`<cmd>.bash` (standard bash completion scripts: the `opts="..."` line plus a
`case "${prev}"` block with `compgen` value lists) placed in a completion
directory are parsed into the same definitions the embedded applet layer
uses. See [Advanced Usage](advanced-usage.md#winuxcmd-applet-completions)
for the file formats and the priority layering — embedded WinuxCmd applet
definitions are the base layer and later definitions override them per
command.

## Priority

Local definitions always win over external providers: embedded applet TOMLs,
bundle definitions, and user completion files are consulted before any
rc-integrated provider fires, so a provider can never shadow a definition
you already have.

## Example: carapace-bin

[carapace-bin](https://github.com/carapace-sh/carapace-bin) is a single
binary carrying community completion specs for ~1200 commands (git, curl,
gh, docker, ...). Its bash integration emits standard bash completion code,
so the standard snippet is all it takes:

```bash
# ~/.niubashrc — requires carapace-bin installed, e.g.
#   wpm install carapace-bin
# (or see https://github.com/carapace-sh/carapace-bin/releases).
# Until carapace is on PATH this block is a no-op.
if command -v carapace >/dev/null 2>&1; then
  source <(carapace _carapace)
fi
```

`niu setup` offers this snippet as an opt-in (default off) — the wizard
writes the same block, it installs nothing.

Static compspecs from the snippet serve candidates today; the snippet's
`complete -F` hooks light up fully when the engine's interactive `-F`/`-C`
resolution lands. Providers that want description-carrying candidates
*today* can bridge through the compdef protocol — carapace's
`carapace <command> fish <words...>` output (`value<TAB>description`, already
prefix-filtered) matches `NIU_COMP_RESULT`'s format line for line:

```bash
# compdef bridge: carapace data through niubash's function protocol
_carapace_bridge() {
    local cmd
    set -- $NIU_COMP_WORDS
    cmd=$1
    NIU_COMP_RESULT="$(carapace "$cmd" fish $NIU_COMP_WORDS 2>/dev/null)"
}
NIU_COMPDEFS="git:_carapace_bridge"
NIU_COMPDEFS="$NIU_COMPDEFS
docker:_carapace_bridge
curl:_carapace_bridge"
```

List the commands you care about — `NIU_COMPDEFS` is plain rc data, edited
any time.
