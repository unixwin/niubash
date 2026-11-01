#!/bin/sh
# Audit the in-process readline C ABI exports of a built niu executable
# (niubash#775, ruline#8).
#
# Usage: scripts/check-readline-abi-exports.sh [path/to/niu]
#        NIU_BIN=... PROFILE=release scripts/check-readline-abi-exports.sh
#
# The symbols in scripts/readline-abi-exports.txt must be in the *dynamic*
# symbol table because bash loadable builtins (`enable -f`) resolve `rl_*` /
# `history_*` against the main executable after `dlopen`.
# tests/readline_abi_exports.rs runs the same audit against Cargo's test
# binary; this script exists for release artifacts, where the whole surface
# must survive the release profile's thin LTO and strip settings.
#
# Exit 0: all symbols present (or the Windows check skipped for lack of
# dumpbin). Exit 1: at least one symbol is missing. Exit 2: no binary / no
# symbol list to inspect.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exports=${READLINE_ABI_EXPORTS:-$script_dir/readline-abi-exports.txt}

if [ "$#" -gt 0 ]; then
    binary=$1
else
    binary=${NIU_BIN:-}
fi

if [ -z "${binary:-}" ]; then
    profile=${PROFILE:-debug}
    case "$(uname -s)" in
        MINGW* | MSYS* | CYGWIN*) suffix=.exe ;;
        *) suffix= ;;
    esac
    binary="target/$profile/niu$suffix"
fi

if [ ! -f "$exports" ]; then
    echo "error: symbol list $exports not found" >&2
    exit 2
fi

if [ ! -f "$binary" ]; then
    echo "error: $binary not found; build niu first (cargo build --locked --bin niu)" >&2
    exit 2
fi

# One `F <name>` / `D <name>` per line; comments and blank lines ignored.
wanted=$(awk '!/^[[:space:]]*(#|$)/ { print $2 }' "$exports")
total=$(printf '%s\n' "$wanted" | awk 'NF' | wc -l | tr -d ' ')
if [ "$total" -eq 0 ]; then
    echo "error: $exports lists no symbols" >&2
    exit 2
fi

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*)
        if ! command -v dumpbin >/dev/null 2>&1; then
            echo "SKIP: dumpbin not on PATH; run from a Visual Studio developer prompt to audit $binary" >&2
            exit 0
        fi
        # `-option` spelling avoids MSYS path mangling of `/option`.
        listing=$(dumpbin -nologo -exports "$binary")
        ;;
    Darwin*)
        # Mach-O executables expose the global defined symbols in their
        # dynamic symbol table; there is no `nm -D`.
        listing=$(nm -gU "$binary")
        ;;
    *)
        # ELF: dynamic symbol table only. Prefer GNU/LLVM nm; fall back to
        # readelf for hosts whose nm lacks -D.
        if command -v nm >/dev/null 2>&1 &&
            listing=$(nm -D --defined-only "$binary" 2>/dev/null); then
            :
        else
            listing=$(readelf --dyn-syms --wide "$binary")
        fi
        ;;
esac

tmp=$(mktemp -d "${TMPDIR:-/tmp}/niu-readline-abi.XXXXXX") || {
    echo "error: cannot create a temporary directory (set TMPDIR)" >&2
    exit 2
}
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

# Normalize to one bare name per line: nm/readelf/dumpbin all print the name
# as one whitespace-separated token; strip the Mach-O leading underscore and
# any ELF @VERSION suffix. Compare exactly so `rl_library_version` cannot
# match a longer name by substring.
printf '%s\n' "$listing" |
    tr ' \t' '\n\n' |
    sed -e 's/^_//' -e 's/@.*$//' |
    grep -E '^[A-Za-z_][A-Za-z0-9_]*$' |
    sort -u >"$tmp/have" || true

printf '%s\n' "$wanted" | awk 'NF' | sort -u >"$tmp/want"
missing=$(comm -23 "$tmp/want" "$tmp/have")
count=$(printf '%s\n' "$missing" | awk 'NF' | wc -l | tr -d ' ')

if [ -n "$missing" ]; then
    printf 'MISSING (%s of %s, %s):\n%s\n' "$count" "$total" "$binary" "$missing" >&2
    exit 1
fi

echo "ok: $total readline ABI symbols exported from $binary"
exit 0
