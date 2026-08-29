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
grep -q '^name = "niubash"' Cargo.toml
grep -q '^name = "niu"$' Cargo.toml
grep -q '^version = "1.0.0"' Cargo.toml
grep -q '^name = "niubash-runtime"' crates/niubash-runtime/Cargo.toml
grep -q '^version = "1.0.0"' crates/niubash-runtime/Cargo.toml
grep -q '^name = "niubash"' Cargo.lock
grep -q '^name = "rubash"' Cargo.lock
grep -A1 '^name = "rubash"' Cargo.lock | grep -q '"1.0.0"'
echo "cargo metadata OK"

echo "rename acceptance: ALL CHECKS PASSED"
