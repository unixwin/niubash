#!/bin/sh
# Brand-cleanliness gate for the winuxsh -> niubash rename.
#
# Fails when an unexpected case-insensitive "winuxsh" reference remains in
# tracked text. Expected (allowlisted) residues:
#   - oh-my-winuxsh              : plugin bundle repo keeps its slug until that
#                                  repository is renamed
#   - WINUXSH_UNSUPPORTED_DEVICE : rubash pseudo-device literal (shared contract)
#   - WINUXSH_SHELL / WINUXSH_SHELL_PATH_STYLE / WINUXSH_ROOT :
#                                  deprecated rubash contract bridge variables
#   - winuxshrc                  : legacy rc fallback chain and migration docs
#   - min_winuxsh / migrate_legacy_winuxsh_rc / kept-as-niu :
#                                  intentional compat shims for the rename
#   - Cargo.lock                 : regenerated; verified separately below
#
# The allowlist is applied with a POSIX case glob (lowercased line) instead of
# grep -E/-F so the gate behaves identically under Git Bash grep and the
# WinuxCmd grep.
# Manifest fields are parsed with awk rather than sed and are never stored as
# shell variables containing `"`: this shell drops double quotes from
# assignment values, which silently broke the earlier sed/grep -A versions.
set -e
cd "$(dirname "$0")/.."

echo "== brand scan =="
if command -v rg >/dev/null 2>&1; then
    hits=$(rg -i -n --no-heading winuxsh --glob '!Cargo.lock' --glob '!target/**' . || true)
else
    hits=$(git grep -i -n winuxsh -- . ':!Cargo.lock' || true)
fi

unexpected=""
allowed_count=0
total=0
if [ -n "$hits" ]; then
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        total=$((total + 1))
        lower=$(printf '%s' "$line" | tr 'A-Z' 'a-z')
        case "$lower" in
            *oh-my-winuxsh*|*winuxsh_unsupported_device*|*winuxsh_shell*|*winuxsh_root*|*winuxshrc*|*min_winuxsh*|*migrate_legacy_winuxsh_rc*|*replace\(*|*check-rename-clean.sh*|*kept-as-niu*|*migrated.contains*)
                allowed_count=$((allowed_count + 1))
                ;;
            *)
                unexpected="$unexpected$line
 "
                ;;
        esac
    done <<EOF
$hits
EOF
fi

if [ -n "$unexpected" ]; then
    echo "FAIL: unexpected winuxsh residue:"
    printf '%s' "$unexpected"
    exit 1
fi
echo "brand scan clean ($allowed_count/$total allowlisted hits)"

echo "== cargo metadata =="

# Every check reports itself instead of letting `set -e` abort the script with
# no message at all; that is what a stale hardcoded version literal used to do.
check() {
    what=$1
    shift
    if "$@"; then
        return 0
    fi
    echo "FAIL: $what"
    exit 1
}

# top_version <manifest>: first top-level `version = "..."` value.
top_version() {
    awk -F'\\042' '/^version = / { gsub(/\r/, "", $0); print $2; exit }' "$1"
}

# lock_pins <package> <version>: true when Cargo.lock records
# `name = "<pkg>"` immediately above `version = "<version>"`.
lock_pins() {
    awk -v want_name="$1" -v want_version="$2" '
        {
            gsub(/\r/, "", $0)
            if (pending) {
                value = $0
                sub(/^version = "/, "", value)
                sub(/".*/, "", value)
                if (value == want_version) found = 1
                exit
            }
            name = $0
            sub(/^name = "/, "", name)
            sub(/".*/, "", name)
            if (name == want_name) pending = 1
        }
        END { exit found ? 0 : 1 }
    ' Cargo.lock
}

root_version=$(top_version Cargo.toml)
runtime_version=$(top_version crates/niubash-runtime/Cargo.toml)
if [ -z "$root_version" ]; then
    echo "FAIL: root Cargo.toml has no top-level version"
    exit 1
fi
if [ -z "$runtime_version" ]; then
    echo "FAIL: crates/niubash-runtime/Cargo.toml has no top-level version"
    exit 1
fi
if [ "$root_version" != "$runtime_version" ]; then
    echo "FAIL: package versions drift (root=$root_version runtime=$runtime_version)"
    exit 1
fi

check 'root package is named niubash' grep -q '^name = "niubash"' Cargo.toml
check 'binary is named niu' grep -q '^name = "niu"$' Cargo.toml
check 'runtime crate is named niubash-runtime' grep -q '^name = "niubash-runtime"' crates/niubash-runtime/Cargo.toml

check 'Cargo.lock records niubash' grep -q '^name = "niubash"' Cargo.lock
check 'Cargo.lock records niubash-runtime' grep -q '^name = "niubash-runtime"' Cargo.lock

rubash_pinned=no
for version in 0.1.0 1.0.0 1.0.1 1.0.2 1.1.0 1.2.0 2.0.0; do
    if lock_pins rubash "$version"; then
        rubash_pinned=$version
        break
    fi
done
if [ "$rubash_pinned" = no ]; then
    echo "FAIL: Cargo.lock does not record a rubash version"
    exit 1
fi

echo "cargo metadata OK (niubash $root_version, rubash $rubash_pinned)"

echo "rename acceptance: ALL CHECKS PASSED"
