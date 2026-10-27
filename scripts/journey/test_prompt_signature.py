#!/usr/bin/env python3
"""Prompt-signature sampler regression tests for golden-journey.py
(niubash#334, release run 38489787748): the P8-S4 check reported an
EMPTY themed-prompt signature for a prompt the raw transcript shows
fully rendered. The viewport read had caught a ConPTY partial
repaint — the newest row above the input row was the top segment's
letterless filler fragment (dots and spaces) — and the old
single-instant walk stopped on it.

The fix made `themed_prompt_signature` a sampler: the newest input
row anchors the block, letterless filler rows are skipped (bounded),
themed structure rows above join, and the best (longest) sample
wins; cross-session comparisons run on the structural profile.

Runs against the real module (importlib, stub sessions only — no
ConPTY, no niu.exe). Needs pywinpty + pyte importable because the
module imports them at top level; the release journey job installs
both before this runs (same contract as test_verdict_exit.py).
Exit 0 = contract holds, 1 = broken.
"""

import importlib.util
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODULE_PATH = HERE / "golden-journey.py"

# A powerline-multiline shaped block: PUA segment row (folder/clock
# glyphs are U+F07C / U+F017, inside the E000-F8FF structure range),
# the partial-repaint filler fragment (spaces + middle dots, no
# letters, no PUA), and the input row.
THEMED_TEXT = "\uf07c ~/repo @host \uf017 9:41\n        ··········\n❯\n"
FLOOR_TEXT = "user@host ~ $\n"


def load_module():
    spec = importlib.util.spec_from_file_location(
        "golden_journey_under_test", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class StubSession:
    """The sampler only ever calls session.text()."""

    def __init__(self, text):
        self._text = text

    def text(self):
        return self._text


def main():
    gj = load_module()
    failures = []

    def expect(label, actual, wanted):
        if actual != wanted:
            failures.append(f"{label}: got {actual!r}, want {wanted!r}")

    # 1. The filed artifact: a letterless filler fragment between the
    # structure row and the input row must be SKIPPED, not stop the
    # walk — the signature is the full two-row block, never empty.
    themed = gj.themed_prompt_signature(StubSession(THEMED_TEXT))
    expect("themed signature length", len(themed), 2)
    expect("themed signature profile",
           gj._signature_profile(themed), ("p", "s"))

    # 2. The same block without the fragment signs identically: the
    # fragment is repaint noise, never part of the comparison.
    clean = gj.themed_prompt_signature(
        StubSession("\uf07c ~/repo @host \uf017 9:41\n❯\n"))
    expect("fragment-free signature equals fragmented",
           gj._signature_profile(clean), gj._signature_profile(themed))
    expect("fragment-free signature text", clean, themed)

    # 3. Floor vs themed: a plain `user@host ~ $` prompt has a
    # different structural profile than the themed block.
    floor = gj.themed_prompt_signature(StubSession(FLOOR_TEXT))
    expect("floor signature length", len(floor), 1)
    expect("floor profile", gj._signature_profile(floor), ("p",))
    if gj._signature_profile(floor) == gj._signature_profile(themed):
        failures.append("floor and themed profiles must differ")

    # 4. Determinism: the same rendered text signs the same way on a
    # second session (the restored == themed comparison in P8-S4).
    again = gj.themed_prompt_signature(StubSession(THEMED_TEXT))
    expect("repeat signature", again, themed)

    # 5. Before the first prompt renders there is no input row: the
    # signature is empty, by definition.
    expect("pre-prompt signature",
           gj.themed_prompt_signature(StubSession("booting…\n")), [])

    if failures:
        print("FAIL:")
        for line in failures:
            print(f"  - {line}")
        return 1
    print("ok: prompt signature sampler skips repaint fragments "
          "(filler skipped, floor != themed, deterministic)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
