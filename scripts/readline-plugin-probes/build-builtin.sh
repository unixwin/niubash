#!/bin/sh
# Build the readline ABI loadable-builtin fixture.
#
# Usage: build-builtin.sh [OUTPUT_DIR]
#        CC=clang build-builtin.sh /tmp/rl-probe
#
# Prints the absolute path of the built shared object on success.  Nothing
# here links libreadline: the .so keeps rl_* undefined (weak probes use
# dlsym), so the host that dlopen()s it decides which readline it talks to
# (GNU bash's libreadline, or niu's in-process export).
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out_dir=${1:-"$script_dir/out"}
cc=${CC:-cc}

if ! command -v "$cc" >/dev/null 2>&1; then
    echo "error: C compiler '$cc' not found (set CC)" >&2
    exit 2
fi

mkdir -p "$out_dir"
out="$out_dir/librl_probe.so"

case "$(uname -s)" in
    Darwin*)
        # Mach-O has no -shared; a loadable bundle is the readline-plugin
        # shape.  Undefined rl_* stay unresolved until the host loads it.
        "$cc" -O2 -g -Wall -Wextra -fPIC -dynamiclib -undefined dynamic_lookup \
            -o "$out" "$script_dir/loadable_builtin.c"
        ;;
    *)
        "$cc" -O2 -g -Wall -Wextra -fPIC -shared -o "$out" \
            "$script_dir/loadable_builtin.c" -ldl
        ;;
esac

printf '%s\n' "$out"
