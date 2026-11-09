# complete-alias (fixture)

Independent completion-recipe shape (cykerway/complete-alias, GPL-3.0-only).
Entry file `complete_alias` sits at the upstream root; the extension-less
name plus the `tests/*.sh` companions are the audited upstream layout
(niubash#171).

## Model (niubash#966)

`__complete_alias` is the `-F` entry point and
`__compcomplete_aliashook` the delegation core: the alias is resolved
to its expansion, and completion runs against the expanded command —
its own `-F` compspec when it has one (COMP_WORDS/COMP_CWORD
rewritten onto the expanded line), filename completion otherwise.

Registration stays manual, as upstream: `complete -F __complete_alias
<alias>` per alias. Sourcing this file must not rewrite global
completion state for sessions that merely load the plugin.

Modeled limits: only `-F` compspecs are delegated (`-C`/`-W`/`-A`
specs fall back to filenames); alias values containing single
quotes are not unescaped beyond the outer pair.
