#!/usr/bin/env bash
# Fixture modeled on rcaloras/bash-preexec (MIT): the canonical wild
# single-file plugin. Shape kept faithful to upstream: PROMPT_COMMAND
# wrapping, preexec/precmd user hooks, preexec_functions/precmd_functions
# arrays. Loading must be byte-faithful to manually sourcing this file
# under GNU bash (§14.6.4).
__bp_imported=1

# User-overridable hooks (upstream names).
preexec() { :; }
precmd() { :; }

preexec_functions=()
precmd_functions=()

__bp_run_preexec() {
  # Re-entry guard (upstream shape): the DEBUG trap fires for every
  # command, including commands a hook itself runs; without it a hook
  # that runs a command would re-enter the runner. GNU suppresses
  # DEBUG inside the handler, so the guard is defense-in-depth on
  # engines that do not.
  if [ -n "${__bp_in_preexec:-}" ]; then
    return 0
  fi
  __bp_in_preexec=1
  local __bp_fn
  for __bp_fn in ${preexec_functions[@]-}; do
    "$__bp_fn"
  done
  __bp_in_preexec=
}

__bp_run_precmd() {
  local __bp_fn
  for __bp_fn in ${precmd_functions[@]-}; do
    "$__bp_fn"
  done
}

if [ -n "${bash_preexec_imported:-}" ]; then
  return 0
fi
bash_preexec_imported="imported"

PROMPT_COMMAND="__bp_run_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"

# Load-time preexec wiring (upstream shape): the DEBUG trap is the
# calling path that fires `preexec_functions` before each command.
# Installed here, at load, so a plainly loaded plugin is fully wired —
# no hand-installed trap in the rc. The `bash_preexec_imported` guard
# above keeps a second sourcing from reinstalling it.
trap '__bp_run_preexec' DEBUG
