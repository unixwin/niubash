#!/bin/sh
# Brand-cleanliness gate for the winuxsh -> niubash rename.
#
# Fails when the worktree's case-insensitive "winuxsh" residue does not
# exactly match the checked-in baseline at
# scripts/check-rename-clean.baseline:
#   - a hit missing from the baseline is new (unexpected) residue -> red
#   - a baseline entry with no matching hit is stale -> red
# The diff is bidirectional on purpose. The previous glob allowlist let
# exemptions rot silently until pristine master went false-red (issue #447);
# an exact baseline can never drift — any residue change turns the gate red
# until the baseline is regenerated.
#
# Baseline regeneration (always review the baseline diff before committing):
#
#     sh scripts/check-rename-clean.sh --regen
#
# The same command is also written into the baseline file's own header and
# into every FAIL message, so the generation method cannot be lost.
#
# Scan universe: `git ls-files -co --exclude-standard` — tracked files plus
# untracked-but-not-ignored files. A fresh CI checkout and a dirty local tree
# therefore produce the same file list, and residue in a not-yet-committed
# file is still caught. There is deliberately one code path: the old
# rg/git-grep split disagreed on dotfiles such as .gitattributes (rg skipped
# it, git grep scanned it), which was part of the #447 false-red. Cargo.lock,
# this script, and the baseline itself are excluded by pathspec.
#
# Baseline entries are "<repo path><TAB><verbatim line text>" — line numbers
# are NOT recorded, so editing a residue-carrying file elsewhere does not
# churn the baseline.
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

baseline=scripts/check-rename-clean.baseline
tab=$(printf '\t')
cr=$(printf '\r')

echo "== brand scan =="

git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "FAIL: not inside a git worktree (baseline scan needs the tree)"
    exit 1
}

# residue_scan: one "<path>:<lineno>:<line>" record per "winuxsh" hit.
# grep -I keeps the scan to text files; -H pins the filename prefix even
# when xargs hands grep a single file.
residue_scan() {
    git ls-files -co --exclude-standard -z -- \
        . ':(exclude)Cargo.lock' \
        ':(exclude)scripts/check-rename-clean.sh' \
        ':(exclude)'"$baseline" \
        | xargs -0 -r grep -i -n -I -H -- winuxsh || true
}

# baseline_key: canonical "<path><TAB><line text>" of one scan record — the
# line number is dropped and a trailing CR is stripped (a CRLF checkout must
# compare equal; the baseline file is read through `tr -d '\r'` too).
baseline_key() {
    path=${1%%:*}
    rest=${1#*:}
    text=${rest#*:}
    printf '%s%s%s\n' "$path" "$tab" "${text%"$cr"}"
}

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

hits=$(residue_scan)
printf '%s\n' "$hits" | while IFS= read -r record; do
    [ -z "$record" ] && continue
    baseline_key "$record"
done | LC_ALL=C sort > "$tmpdir/scan"

if [ "${1:-}" = "--regen" ]; then
    {
        printf '%s\n' \
            '# winuxsh residue baseline for scripts/check-rename-clean.sh' \
            '# GENERATED — do not edit by hand. Regenerate with:' \
            '#     sh scripts/check-rename-clean.sh --regen' \
            '# Format: <repo-relative path><TAB><verbatim line text of the hit>'
        cat "$tmpdir/scan"
    } > "$baseline"
    count=$(wc -l < "$tmpdir/scan" | tr -d ' \r')
    echo "baseline regenerated: $baseline ($count entries)"
    exit 0
fi

if [ -f "$baseline" ]; then
    tr -d '\r' < "$baseline" \
        | sed -e '/^#/d' -e '/^[[:space:]]*$/d' \
        | LC_ALL=C sort > "$tmpdir/base"
    missing=no
else
    : > "$tmpdir/base"
    missing=yes
fi

if cmp -s "$tmpdir/scan" "$tmpdir/base"; then
    count=$(wc -l < "$tmpdir/scan" | tr -d ' \r')
    echo "brand scan clean ($count baseline hits)"
else
    comm -23 "$tmpdir/scan" "$tmpdir/base" > "$tmpdir/new"
    comm -13 "$tmpdir/scan" "$tmpdir/base" > "$tmpdir/stale"
    echo "FAIL: winuxsh residue does not match $baseline"
    [ "$missing" = yes ] && echo "  (baseline file is missing — regenerate it, see below)"
    if [ -s "$tmpdir/new" ]; then
        echo "unexpected residue (hits not in the baseline):"
        printf '%s\n' "$hits" | while IFS= read -r record; do
            [ -z "$record" ] && continue
            path=${record%%:*}
            rest=${record#*:}
            text=${rest#*:}
            # < /dev/null: the WinuxCmd grep slurps the loop's pipe stdin
            # even with a file operand, which would end the loop after the
            # first record.
            if grep -qxF "${path}${tab}${text%"$cr"}" "$tmpdir/new" < /dev/null; then
                printf '  + %s\n' "${record%"$cr"}"
            fi
        done
    fi
    if [ -s "$tmpdir/stale" ]; then
        echo "stale baseline entries (residue no longer present):"
        while IFS= read -r key; do
            [ -z "$key" ] && continue
            printf '  - %s: %s\n' "${key%%"$tab"*}" "${key#*"$tab"}"
        done < "$tmpdir/stale"
    fi
    echo "if this residue change is intentional, regenerate the baseline:"
    echo "    sh scripts/check-rename-clean.sh --regen"
    exit 1
fi

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
for version in 0.1.0 1.0.0 1.0.1 1.0.2 1.1.0 1.2.0 1.2.1 1.2.2 1.2.3 1.2.4 1.2.5 1.3.0 1.3.1 1.3.2 1.3.3 1.3.4 1.4.0 1.4.1 1.4.2 1.4.3 1.5.0 2.0.0; do
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
