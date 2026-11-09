# fzf-git.sh (fixture)

Wild single-file plugin shape (junegunn/fzf-git.sh). One sourceable
file, `fzf-git.sh`; upstream binds its widgets through fzf's own
key-bindings channel, so nothing binds at source time.

## Offline model (niubash#966)

Upstream pipes `git ls-files` through the fzf binary for interactive
selection; this fixture never invokes fzf (the binary comes from the
user's package manager). Instead the widget is deterministic and
offline:

- `_fzf_git_files [query]` — the candidate list: the repo's tracked
  files in `git ls-files` order, optionally filtered to entries
  containing the query as a literal substring. Empty and silent
  outside a repo.
- `fzf_git_files [query]` — prints the candidate list and, when the
  line editor is present (`READLINE_LINE` set), inserts the first
  candidate into the line being edited — the offline stand-in for
  interactive selection.

Usage in a session is the same as upstream: bind the widget to a key,
e.g. `bind -x '"\C-gf": fzf_git_files'`.
