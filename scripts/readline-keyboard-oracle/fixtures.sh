#!/bin/sh
# Deterministic fixtures for the readline keyboard oracle.
#
# Usage: fixtures.sh <case-root>
#
# Creates:
#   <case-root>/home/.history   controlled history (three entries)
#   <case-root>/home/            isolated HOME
#   <case-root>/inputrc          controlled INPUTRC for GNU readline / niu
#   <case-root>/work/            cwd with stable completion candidates
#   <case-root>/dump/            where the bind -x hook writes line/point
#
# Nothing here touches the real HOME, HISTFILE, INPUTRC, or cwd.

set -eu

root=${1:?usage: fixtures.sh <case-root>}

rm -rf "$root"
mkdir -p "$root/home" "$root/work" "$root/dump"

# Filenames chosen so TAB has a useful common prefix:
#   "al"    -> alpha.txt, alpine.txt (common prefix "alp")
#   "alph"  -> alpha.txt only
#   "al*"   -> M-g glob target
: >"$root/work/alpha.txt"
: >"$root/work/alpine.txt"
: >"$root/work/beta.txt"

cat >"$root/home/.history" <<'EOF'
alpha one
beta two
gamma three
EOF

cat >"$root/inputrc" <<'EOF'
# GNU readline 5.3 variables. niu mirrors the GNU surface it implements;
# unknown lines are inert on either side.
set editing-mode emacs
set enable-bracketed-paste on
set bell-style none
set keyseq-timeout 500
set completion-ignore-case off
set show-all-if-ambiguous off
set show-all-if-unmodified off
set page-completions off
EOF
