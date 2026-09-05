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
#
# Manifest fields are parsed with awk. Three quirks of this shell force the
# exact forms below, so keep them as written:
#   - `-F'\042'` carries a single backslash: awk resolves that octal escape to
#     the quote field separator, so the script never spells `"` in a pattern.
#     A doubled backslash turns the separator into a literal string and every
#     field comparison silently stops matching.
#   - `print` emits CRLF here, so a captured version keeps a trailing CR and
#     must be trimmed with `${var%"$cr"}` before it is compared.
#   - double quotes do not survive this shell in command substitution or in
#     assignment values, so versions are never stored as quoted strings and no
#     awk program may rely on an empty-string literal (`""` gets dropped).
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

cr=$(printf '\r')

# top_version <manifest>: first top-level `version = "..."` value, CR-free.
top_version() {
    version=$(awk -F'\042' '/^version = /{print $2; exit}' "$1")
    printf '%s' "${version%"$cr"}"
}

# lock_pins <package> <version>: true when Cargo.lock records
# `name = "<pkg>"` immediately above `version = "<version>"`.
lock_pins() {
    awk -v want_name="$1" -v want_version="$2" -F'\042' '
        $1 == "name = " && $2 == want_name {
            getline
            if ($1 == "version = " && $2 == want_version) found = 1
            exit
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
check "Cargo.lock pins niubash $root_version" lock_pins niubash "$root_version"
check "Cargo.lock pins niubash-runtime $runtime_version" lock_pins niubash-runtime "$runtime_version"

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
