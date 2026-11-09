# bash-preexec (fixture)

Wild single-file plugin shape (rcaloras/bash-preexec). One sourceable
file, `bash-preexec.sh`; loading registers a PROMPT_COMMAND wrapper and
the preexec/precmd hook arrays exactly as upstream does — and, since
niubash#965, installs the DEBUG trap on `__bp_run_preexec` at load
time (with the `__bp_in_preexec` re-entry guard), so a plainly loaded
plugin fires `preexec_functions` before every command with no
hand-wired trap in the rc.
