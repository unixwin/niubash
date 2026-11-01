#!/bin/sh
# Audit the in-process readline C ABI exports of a built niu executable
# (niubash#775, ruline#8).
#
# Usage: scripts/check-readline-abi-exports.sh [path/to/niu]
#        NIU_BIN=... PROFILE=release scripts/check-readline-abi-exports.sh
#
# The `readline_abi` probe symbols must be in the *dynamic* symbol table
# because bash loadable builtins (`enable -f`) resolve `rl_*` against the
# main executable after `dlopen`. `tests/readline_abi_exports.rs` runs the
# same check against Cargo's test binary; this script exists for release
# artifacts, where the probe must survive the release profile's thin LTO and
# strip settings.
#
# Exit 0: all symbols present (or the Windows check skipped for lack of
# dumpbin). Exit 1: a symbol is missing. Exit 2: no binary to inspect.
set -eu

symbols="rl_readline_version rl_library_version"

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

if [ ! -f "$binary" ]; then
    echo "error: $binary not found; build niu first (cargo build --locked --bin niu)" >&2
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

status=0
for symbol in $symbols; do
    # nm/readelf/dumpbin all print the name as one whitespace-separated
    # token; strip the Mach-O leading underscore and compare exactly so
    # `rl_library_version` cannot match a longer name by substring.
    if printf '%s\n' "$listing" |
        tr ' \t' '\n\n' |
        sed 's/^_//' |
        grep -qx "$symbol"; then
        echo "ok: $symbol"
    else
        echo "MISSING: $symbol ($binary)" >&2
        status=1
    fi
done

if [ "$status" -ne 0 ]; then
    echo "--- symbol listing ---" >&2
    printf '%s\n' "$listing" >&2
fi
exit "$status"
