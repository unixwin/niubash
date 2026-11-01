#!/usr/bin/env bash
# Completion-options matrix: styles x interactions, precedence, compdefs.
#
# Usage:
#   scripts/completion-options-matrix/run.sh
#   scripts/completion-options-matrix/run.sh --filter select
#   scripts/completion-options-matrix/run.sh --record-baseline
#
# Environment:
#   NIU_BIN   binary under test (default: <repo>/target/debug/niu)
#   PYTHON    python3 interpreter (default: python3)
#
# Exit status: 0 when every case is PASS/MISSING/SKIP, 1 on any FAIL.
# No network, no Rust build, no third-party Python packages.

set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
python=${PYTHON:-python3}
if ! command -v "$python" >/dev/null 2>&1; then
    echo "completion-options-matrix: $python not found" >&2
    exit 2
fi
exec "$python" "$here/matrix.py" "$@"
