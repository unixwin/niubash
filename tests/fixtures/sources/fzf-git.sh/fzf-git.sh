#!/usr/bin/env bash
# Fixture modeled on junegunn/fzf-git.sh (MIT): single script at the
# upstream root (`fzf-git.sh`, audited 2026-10-02, wt61 pattern). Widget
# functions only — upstream binds them through fzf's own key-bindings
# channel; nothing binds at source time.
fzf_git_fixture_imported=1

# Deterministic offline candidate list (niubash#966): the tracked
# files of the current repo, in `git ls-files` order, optionally
# filtered to entries containing the query as a literal substring.
# Empty (and silent) outside a repo — `git ls-files` fails there and
# the failure is swallowed, exactly like an empty candidate set.
_fzf_git_files() {
  local query="$1"
  local f
  git ls-files 2>/dev/null | while IFS= read -r f; do
    case "$query" in
      "") printf '%s\n' "$f" ;;
      *) case "$f" in *"$query"*) printf '%s\n' "$f" ;; esac ;;
    esac
  done
}

fzf_git_files() {
  # Print the candidate list (the observable output) and, when the
  # line editor is present, insert the first candidate into the line
  # being edited — the offline stand-in for upstream's interactive
  # selection, which needs the fzf binary this fixture never invokes.
  local list first_line
  list="$(_fzf_git_files "$1")"
  if [ -n "$list" ]; then
    printf '%s\n' "$list"
    if [ -n "${READLINE_LINE+x}" ]; then
      first_line="${list%%$'\n'*}"
      READLINE_LINE="$first_line"
    fi
  fi
}
