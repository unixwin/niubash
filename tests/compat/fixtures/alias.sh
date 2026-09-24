# Aliases do not expand in non-interactive shells by default (GNU shell.c
# init_interactive_script keeps expand_aliases off); enable the option the
# same way a real script would before exercising alias expansion.
shopt -s expand_aliases
alias ll="echo LIST"
ll
