#!/bin/sh
# Regenerate scripts/readline-abi-exports.txt from a built ruline
# libreadline (niubash#775, ruline#8).
#
# Usage: scripts/gen-readline-abi-exports.sh path/to/libreadline.so
#        RULINE_LIB=... scripts/gen-readline-abi-exports.sh
#
# Build it first in the ruline checkout:
#   cargo build -p readline-abi            # -> target/debug/libreadline.so
#
# Kind is derived from the ELF dynamic symbol type: `T` (text) is a function
# (`F`), `D`/`B`/`R` are data (`D`). The list is sorted by symbol name so a
# ruline ABI bump produces a reviewable diff.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=$script_dir/readline-abi-exports.txt
lib=${1:-${RULINE_LIB:-}}

if [ -z "${lib:-}" ]; then
    echo "usage: $0 path/to/libreadline.so (or set RULINE_LIB)" >&2
    exit 2
fi
if [ ! -f "$lib" ]; then
    echo "error: $lib not found" >&2
    exit 2
fi

tmp=$(mktemp "${TMPDIR:-/tmp}/readline-abi-exports.XXXXXX") || exit 2
trap 'rm -f "$tmp"' EXIT HUP INT TERM

nm -D --defined-only "$lib" |
    awk '$2 == "T" { print "F " $3 }
         $2 == "D" || $2 == "B" || $2 == "R" { print "D " $3 }' |
    sort -k2 >"$tmp"

count=$(wc -l <"$tmp" | tr -d ' ')
if [ "$count" -lt 100 ]; then
    echo "error: only $count symbols in $lib; refusing to truncate the list" >&2
    exit 1
fi

{
    printf '%s\n' '# In-process GNU readline C ABI surface exported by niu (niubash#775, ruline#8).'
    printf '%s\n' '#'
    printf '%s\n' '# Generated from a built ruline libreadline:'
    printf '%s\n' '#   nm -D --defined-only target/debug/libreadline.so'
    printf '%s\n' '# Kind: F = function (T), D = data (D/B/R). Keep this file sorted by name.'
    printf '%s\n' '# Regenerate with scripts/gen-readline-abi-exports.sh (niubash repo).'
    printf '\n'
    cat "$tmp"
} >"$out"

echo "wrote $count symbols to $out"
