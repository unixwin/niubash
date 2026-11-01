#!/usr/bin/env python3
"""Frame-level regression for niubash#778 (TAB #1 + grid arrows).

Spawns the real niu under a PTY, types a prefix, and captures the raw
bytes of the frame the terminal receives.  Two independent observations
are checked per scenario:

  * the line, through the existing `bind -x` READLINE_LINE hook, and
  * the grid cell, through the SGR sequence the painter emits directly
    before the candidate's text (the menu-select selection style is
    green + reverse).

This is deliberately not a unit-level `Menu::menu_string` call: the bug
(#778) was visible only in the composed frame — the line and the grid
were painted from different state.  The scenarios pin:

  TAB             -> candidate 0 in the line and selected in the grid
  TAB Right       -> same-row candidate in the next column, selected
  TAB Right Left  -> candidate 0 again
  TAB Down        -> next-row candidate (column-major), selected
  TAB Down Up     -> candidate 0 again (one row up)
  TAB Up          -> zsh's top-edge `goto left` transition, landing on
                     the bottom-right cell (wrap==2 adjustment)

Usage:
  scripts/readline-keyboard-oracle/select_first_tab_frame.py
  NIU_BIN=/path/to/niu scripts/readline-keyboard-oracle/select_first_tab_frame.py

Exit status: 0 all scenarios pass, 1 a mismatch, 2 harness error.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import sys
import time
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
sys.path.insert(0, str(SCRIPT_DIR))

from pty_compare import (  # noqa: E402
    DOWN,
    LEFT,
    PROMPT,
    RIGHT,
    TAB,
    UP,
    Case,
    PtySession,
    build_env,
    clear_dump,
    install_hook,
    run_fixtures,
    wait_dump,
)

WORK = SCRIPT_DIR / ".rl-oracle-work" / "select-first-tab-frame"
DUMP_KEY = b"\x18\x04"  # C-x C-d: the bind -x READLINE_LINE hook.
NIU_CASE = Case(
    "frame/select_first_tab",
    "frame",
    b"",
    b"",
    "",
    niu_env={"NIU_COMPLETION_STYLE": "column"},
)

# Four long names sharing "al": at 80 columns the packed layout is 2
# columns x 2 rows (column-major):
#
#   alpha-…   alps-…
#   alpine-…  alto-…
#
# so Right/Left move to another column and Up/Down to another row.
# A prefix the fixture files (alpha.txt / alpine.txt / beta.txt) do not
# share keeps this list exactly four candidates.
CANDIDATES = [
    "zq-alpha-aaaaaaaaaaaaaaaaaaaa.txt",
    "zq-alpine-bbbbbbbbbbbbbbbbbbbb.txt",
    "zq-alps-cccccccccccccccccccc.txt",
    "zq-alto-dddddddddddddddddddd.txt",
]


class FrameFailure(RuntimeError):
    pass


SGR = re.compile(rb"\x1b\[([0-9;]*)m")


def sgr_is_selected(window: bytes) -> bool:
    """Whether the last SGR sequence in `window` is green + reverse."""
    matches = list(SGR.finditer(window))
    if not matches:
        return False
    values = set()
    for param in matches[-1].group(1).split(b";"):
        try:
            values.add(int(param or b"0"))
        except ValueError:
            return False
    return 7 in values and 32 in values


def last_cell_is_selected(frame: bytes, text: str) -> bool:
    """The last paint of `text` is preceded by the selection SGR.

    The painter writes the input line first and the grid after it, so
    the last occurrence of a candidate in the frame is its grid cell.
    """
    needle = text.encode()
    index = frame.rfind(needle)
    if index < 0:
        return False
    return sgr_is_selected(frame[max(0, index - 128) : index])


def make_work_tree() -> None:
    if WORK.exists() and not KEEP:
        shutil.rmtree(WORK)
    run_fixtures(WORK, "sh")
    for name in CANDIDATES:
        (WORK / "work" / name).touch()


def capture(keys: bytes, settle: float) -> tuple[bytes, bytes]:
    """Type `cat al`, send `keys`, return (frame-after-keys, READLINE_LINE)."""
    env = build_env(WORK, "niu", NIU_CASE)
    session = PtySession([NIU_BIN], WORK / "work", env)
    try:
        session.wait_for(PROMPT, 10.0, "prompt")
        install_hook(session, WORK, settle)
        clear_dump(WORK)
        session.send(b"cat zq")
        session.drain_for(0.20)
        mark = len(session.transcript)
        session.send(keys)
        session.drain_for(settle)
        frame = bytes(session.transcript[mark:])
        session.send(DUMP_KEY)
        line, _point = wait_dump(WORK, session, 8.0)
        return frame, line
    finally:
        session.close()


def check(name: str, keys: bytes, expected: str, settle: float) -> None:
    frame, line = capture(keys, settle)
    got = line.decode("utf-8", "replace").rstrip()
    want = f"cat {expected}"
    if got != want:
        raise FrameFailure(f"{name}: line is {got!r}, expected {want!r}")
    if not last_cell_is_selected(frame, expected):
        raise FrameFailure(
            f"{name}: grid cell {expected!r} is not painted selected; "
            f"tail={frame[-400:]!r}"
        )
    print(f"PASS  {name:<28} line={got!r}  grid cell selected")


def main(argv: list[str]) -> int:
    global NIU_BIN, KEEP
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--niu-bin",
        default=os.environ.get("NIU_BIN", str(REPO_ROOT / "target" / "debug" / "niu")),
    )
    parser.add_argument("--settle", type=float, default=0.60)
    parser.add_argument("--keep-work", action="store_true")
    opts = parser.parse_args(argv)

    NIU_BIN = opts.niu_bin
    KEEP = opts.keep_work
    if not (os.path.isfile(NIU_BIN) and os.access(NIU_BIN, os.X_OK)):
        print(f"select_first_tab_frame: niu binary not executable: {NIU_BIN}", file=sys.stderr)
        return 2

    make_work_tree()
    started = time.monotonic()
    try:
        check("tab_first_candidate", TAB, CANDIDATES[0], opts.settle)
        check("right_next_column", TAB + RIGHT, CANDIDATES[2], opts.settle)
        check("right_left_back", TAB + RIGHT + LEFT, CANDIDATES[0], opts.settle)
        check("down_next_row", TAB + DOWN, CANDIDATES[1], opts.settle)
        check("down_up_back", TAB + DOWN + UP, CANDIDATES[0], opts.settle)
        check("up_top_wrap", TAB + UP, CANDIDATES[3], opts.settle)
    except FrameFailure as exc:
        print(f"FAIL  {exc}", file=sys.stderr)
        return 1
    finally:
        if not KEEP and WORK.exists():
            shutil.rmtree(WORK, ignore_errors=True)
    print(f"PASS  6/6 scenarios in {time.monotonic() - started:.1f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
