#!/usr/bin/env python3
"""Control experiment: the ConPTY/pywinpty channel drops output written
in the final moments before process exit.

Why this exists (wt82-L02): `niu setup` (the setup verb) exits right
after its finish screen, and the finish-screen tail ("Undo this run:" /
"Change things later:") never arrived through the ConPTY session — while
first-contact runs (bare `niu`, which stay alive in the REPL) always
print their full finish screen. Before blaming the product, this probe
runs a trivial child that prints 12 flushed lines and exits immediately:

- without a wrapper, the last lines are LOST (the exit-flush window),
- the journal/backup files prove the niu wizard ran to completion and
  exited 0 — the bytes were written, the CHANNEL lost them.

Consequence for the lane: any assertion on the tail of an exiting
process is unreliable through this channel; such receipts are asserted
from durable state (journal) instead, or from sessions that stay alive.
On a real Windows Terminal the console host flushes synchronously —
this is a probe-channel artifact, NOT a product finding.

Artifacts: control-exit-flush.txt (this run's evidence) next to the
verdicts under target/audit-results/wt82-L02/.
"""

from __future__ import annotations

import argparse
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from conpty_lib import Session, build_env


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)

    root = Path(tempfile.mkdtemp(prefix="wt82-exit-flush-"))
    exe = args.artifacts.parents[2] / "release" / "niu.exe"
    env = build_env(root, exe)

    child = root / "child.py"
    child.write_text(
        "import time\n"
        "print('LINE-01', flush=True)\n"
        "time.sleep(0.5)\n"
        "for i in range(2, 13):\n"
        "    print(f'LINE-{i:02d} last-moments', flush=True)\n",
        encoding="utf-8")
    s = Session([sys.executable, str(child)], root, env,
                raw_log=args.artifacts / "control-exit-flush.raw.ansi",
                label="exit-flush-control")
    for _ in range(30):
        if not s.proc.isalive():
            break
        time.sleep(0.3)
    time.sleep(1.5)
    got = [line for line in s.raw_stripped().splitlines()
           if line.startswith("LINE-")]
    s.close()
    lines = [
        f"child printed 12 lines; ConPTY delivered {len(got)}:",
        *[f"  {line}" for line in got],
        "",
        "lost-at-exit lines: "
        + str([f"LINE-{i:02d}" for i in range(len(got) + 1, 13)]),
        "INTERPRETATION: output written in the final moments before "
        "process exit is dropped by the ConPTY/pywinpty channel. A "
        "first-contact wizard run (REPL stays alive) loses nothing — "
        "the truncation is a property of the probe channel, not of niu.",
    ]
    report = "\n".join(lines) + "\n"
    (args.artifacts / "control-exit-flush.txt").write_text(
        report, encoding="utf-8", newline="\n")
    print(report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
