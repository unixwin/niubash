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
  local __bp_fn
  for __bp_fn in ${preexec_functions[@]-}; do
    "$__bp_fn"
  done
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
