#!/usr/bin/env bash
# Readline keyboard oracle: GNU bash/readline vs the pinned niu, under PTYs.
#
# Usage:
#   scripts/readline-keyboard-oracle/run.sh
#   scripts/readline-keyboard-oracle/run.sh --filter complete
#   scripts/readline-keyboard-oracle/run.sh --record-baseline
#
# Environment:
#   BASH_BIN  GNU bash to use (default: bash from PATH, else the Termux bash)
#   NIU_BIN   niu binary under test (default: the pinned debug build below)
#   PYTHON    python3 interpreter (default: python3)
#
# Exit status: 0 when every case is PASS/KNOWN/SKIP; 1 on any FAIL; 2 on a
# harness error.  No network, no Rust build, no fzf/ble.sh.

set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

python=${PYTHON:-python3}
if ! command -v "$python" >/dev/null 2>&1; then
    echo "readline-keyboard-oracle: $python not found" >&2
    exit 2
fi

default_niu=/data/data/com.termux/files/home/projects/niubash/target/debug/niu
if [ -n "${NIU_BIN:-}" ]; then
    niu=$NIU_BIN
elif [ -x "$default_niu" ]; then
    niu=$default_niu
else
    niu=$default_niu
fi

if [ -n "${BASH_BIN:-}" ]; then
    bash_bin=$BASH_BIN
elif command -v bash >/dev/null 2>&1; then
    bash_bin=$(command -v bash)
else
    bash_bin=/data/data/com.termux/files/usr/bin/bash
fi

exec "$python" "$here/pty_compare.py" \
    --bash-bin "$bash_bin" \
    --niu-bin "$niu" \
    "$@"
