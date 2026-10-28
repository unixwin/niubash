#!/usr/bin/env bash
# Four-way same-machine differential harness (RC audit phase 2, item 3).
#
# Runs every cell script (builtins-only special-path / redirect shapes)
# under up to four Windows shells on THIS machine — niu (the build from
# this checkout), Git Bash, MSYS2 bash, Cygwin bash — and records
# stdout / stderr / exit code verbatim per cell per shell, plus a
# flattened comparison table. The harness itself runs under Git Bash;
# cells are passed as bare relative filenames from a per-cell working
# directory so no cross-shell path conversion enters the measurement.
#
# Inputs (env): NIU_EXE, GITBASH_EXE, MSYS2_EXE, CYGWIN_EXE — any may be
# empty/missing, that shell is then recorded as UNAVAILABLE.
# Output: $SIM_OUT (default ./out) with <shell>/<cell>/{stdout,stderr,rc}
# and table.md at the root.

set -u
cd "$(dirname "$0")"
CELLS_DIR="$PWD/cells"
OUT="${SIM_OUT:-$PWD/out}"
rm -rf "$OUT"
mkdir -p "$OUT"

# Normalize any Windows-form input paths to the harness's POSIX form.
for v in NIU_EXE GITBASH_EXE MSYS2_EXE CYGWIN_EXE; do
    if [ -n "${!v:-}" ]; then
        printf -v "$v" '%s' "$(cygpath -u "${!v}" 2>/dev/null || printf '%s' "${!v}")"
    fi
done

shell_names=()
declare -A shell_bin=()
add_shell() {
    local name="$1" bin="${2:-}"
    if [ -n "$bin" ] && [ -f "$bin" ]; then
        shell_names+=("$name")
        shell_bin[$name]="$bin"
    else
        echo "$name UNAVAILABLE (bin=$bin)" >>"$OUT/availability.txt"
    fi
}
add_shell niu "${NIU_EXE:-}"
add_shell gitbash "${GITBASH_EXE:-}"
add_shell msys2 "${MSYS2_EXE:-}"
add_shell cygwin "${CYGWIN_EXE:-}"
printf '%s\n' "${shell_names[@]}" >"$OUT/shells-run.txt"

run_cell() {
    local name="$1" cell="$2"
    local dir="$OUT/$name/$cell"
    mkdir -p "$dir"
    cp "$CELLS_DIR/$cell" "$dir/cell.sh"
    local bin="${shell_bin[$name]}"
    (
        cd "$dir" || exit 97
        case "$name" in
            msys2) MSYSTEM=MINGW64 timeout 15 "$bin" cell.sh >stdout.txt 2>stderr.txt ;;
            *) timeout 15 "$bin" cell.sh >stdout.txt 2>stderr.txt ;;
        esac
        echo $? >rc.txt
    )
}

for cell in "$CELLS_DIR"/*.sh; do
    cell="$(basename "$cell")"
    for name in "${shell_names[@]}"; do
        run_cell "$name" "$cell"
    done
done

# Flattened comparison table: one row per cell/shell, payloads with
# newlines folded and truncated — the verbatim bytes stay in the files.
flat() {
    tr '\r\n' '~|' <"$1" | cut -c1-160
}
{
    echo "# four-way differential — raw table (Windows runner, same machine)"
    echo
    echo "| cell | shell | rc | stdout | stderr |"
    echo "|---|---|---|---|---|"
    for cell in "$CELLS_DIR"/*.sh; do
        cell="$(basename "$cell")"
        for name in "${shell_names[@]}"; do
            d="$OUT/$name/$cell"
            printf '| %s | %s | %s | %s | %s |\n' \
                "$cell" "$name" "$(cat "$d/rc.txt" 2>/dev/null)" \
                "$(flat "$d/stdout.txt" 2>/dev/null)" \
                "$(flat "$d/stderr.txt" 2>/dev/null)"
        done
    done
} >"$OUT/table.md"

echo "harness done: shells=${shell_names[*]} out=$OUT"
